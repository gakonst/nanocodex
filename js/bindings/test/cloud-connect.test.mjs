import assert from "node:assert/strict";
import { test } from "node:test";

import { Actions, Client, Dialog, Errors, Transport } from "../cloud/index.mjs";
import { Voice } from "../browser/index.mjs";
import { isManagedReadPath, projectAgentObservations } from "../cloud/actions/agent.mjs";
import { connectionFromWire } from "../cloud/internal.mjs";
import { managedBrowserVoiceTransport } from "../managed/internal.mjs";
import { resolveResponsesTransport } from "../runtime/responses-transport.mjs";
import {
  hostedAppToolCatalog,
  hostedToolCatalogDigest,
} from "../tools/hostedCatalog.mjs";

test("Connect opens a ticketed local WASM model socket without exposing its grant token", async () => {
  const requests = [];
  const sockets = [];
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const client = Client.create({
    appId: "atlas-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.from({
      key: "ticketed-model",
      name: "ticketed-model",
      type: "ticketed-model",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async request(request) {
            requests.push(request);
            return { ticket: "one-time-ticket", expires_in: 60 };
          },
        };
      },
    }),
  });
  client._setSessionToken("grant-session-secret");
  const connection = connectionFromWire(testConnectionWire({
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent", "chatgpt"],
  }));
  const OriginalWebSocket = globalThis.WebSocket;
  globalThis.WebSocket = class {
    constructor(url, protocols) {
      this.url = String(url);
      this.protocols = protocols;
      sockets.push(this);
    }
  };
  try {
    const model = resolveResponsesTransport(client.model.transport({ connection }));
    const socket = await model.createWebSocket(
      "wss://api.openai.com/v1/responses",
      "019fc927-b280-79a7-8445-1b9996ad2fb0",
      { authorization: "host_managed", turnState: "retained-turn-state" },
    );
    assert.strictEqual(socket, sockets[0]);
  } finally {
    globalThis.WebSocket = OriginalWebSocket;
  }

  assert.deepEqual(requests, [{
    method: "POST",
    path: `/v1/grants/${connection.grant.id}/model/ticket`,
    body: {
      session_id: "019fc927-b280-79a7-8445-1b9996ad2fb0",
      turn_state: "retained-turn-state",
    },
    headers: { authorization: "Bearer grant-session-secret" },
  }]);
  const socketUrl = new URL(sockets[0].url);
  assert.equal(socketUrl.origin, "wss://connect.example");
  assert.equal(socketUrl.pathname, `/v1/grants/${connection.grant.id}/model`);
  assert.equal(socketUrl.searchParams.get("app_id"), "atlas-workspace");
  assert.equal(socketUrl.searchParams.get("session_id"), "019fc927-b280-79a7-8445-1b9996ad2fb0");
  assert.equal(socketUrl.searchParams.has("ticket"), false);
  assert.deepEqual(sockets[0].protocols, [
    "nanocodex-connect-v1",
    "nanocodex-ticket.one-time-ticket",
  ]);
  assert.equal(socketUrl.href.includes("grant-session-secret"), false);
});

test("Connect HTTP transport binds every API request to the configured app ID", async () => {
  const fetches = [];
  const transport = Transport.http("https://connect.example", {
    credentials: "include",
    async fetch(input, init) {
      fetches.push({ request: new Request(input, init), init });
      return Response.json({ ok: true });
    },
  }).setup({ appId: "consumer-app" });

  await transport.fetch("https://connect.example/v1/direct", { credentials: "omit" });
  await transport.request({ method: "GET", path: "/v1/control" });

  assert.equal(fetches[0].request.headers.get("x-nanocodex-app-id"), "consumer-app");
  assert.equal(fetches[0].init.credentials, "omit");
  assert.equal(fetches[1].request.headers.get("x-nanocodex-app-id"), "consumer-app");
  assert.equal(fetches[1].init.credentials, "include");
});

test("Connect HTTP transport sends POST bodies through the native fetch boundary without wrapping them", async () => {
  const fetches = [];
  const transport = Transport.http("https://connect.example", {
    async fetch(input, init) {
      fetches.push({ input, init });
      return Response.json({ ok: true });
    },
  }).setup({ appId: "mobile-app" });

  const body = JSON.stringify({ input: "hello from Safari" });
  await transport.fetch("https://connect.example/v1/agents/agent-1/turns", {
    method: "POST",
    headers: {
      authorization: "Bearer session",
      "content-type": "application/json",
      "idempotency-key": "turn-1",
    },
    body,
  });

  assert.equal(fetches[0].input, "https://connect.example/v1/agents/agent-1/turns");
  assert.equal(fetches[0].init.method, "POST");
  assert.equal(fetches[0].init.body, body);
  assert.equal(fetches[0].init.headers.get("authorization"), "Bearer session");
  assert.equal(fetches[0].init.headers.get("content-type"), "application/json");
  assert.equal(fetches[0].init.headers.get("idempotency-key"), "turn-1");
  assert.equal(fetches[0].init.headers.get("x-nanocodex-app-id"), "mobile-app");
});

test("Connect durable prompts reach native fetch without wrapping their POST body in Request", async () => {
  const fetches = [];
  const agentId = "019fc927-b280-79a7-8445-1b9996ad2fb0";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const client = Client.create({
    appId: "mobile-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.http("https://connect.example", {
      async fetch(input, init) {
        fetches.push({ input, init });
        return Response.json({
          turn_id: "mobile-turn",
          state: "completed",
          accepted_cursor: "1",
          terminal_cursor: "2",
          terminal: {
            type: "turn_completed",
            final_message: "received",
            usage: null,
            citations: [],
          },
        });
      },
    }),
  });
  client._setSessionToken("grant-session-test");
  const connection = connectionFromWire(testConnectionWire({
    agentId,
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent", "agent.output.final", "chatgpt"],
  }));
  const agent = await client.agent.create({ connection });
  const turn = agent.turn.prompt({
    id: "mobile-turn",
    idempotencyKey: "mobile-turn",
    input: "hello from Mobile Safari",
  });

  assert.equal((await turn.result()).finalMessage, "received");
  assert.equal(fetches.length, 1);
  assert.equal(fetches[0].input instanceof Request, false);
  assert.equal(
    new URL(fetches[0].input).pathname,
    `/v1/grants/${connection.grant.id}/agents/${agentId}/turns`,
  );
  assert.equal(fetches[0].init.method, "POST");
  assert.equal(fetches[0].init.body, JSON.stringify({
    id: "mobile-turn",
    input: "hello from Mobile Safari",
  }));
  assert.equal(fetches[0].init.headers.get("authorization"), "Bearer grant-session-test");
  assert.equal(fetches[0].init.headers.get("content-type"), "application/json");
  assert.equal(fetches[0].init.headers.get("idempotency-key"), "mobile-turn");
  assert.equal(fetches[0].init.headers.get("x-nanocodex-app-id"), "mobile-workspace");
});

test("Connect opens its grant-provisioned durable agent without a redundant state probe", async () => {
  const requests = [];
  const agentId = "019fc927-b280-79a7-8445-1b9996ad2fb0";
  const sessionId = "019fc927-b280-79a7-8445-1b9996ad2fb1";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const client = Client.create({
    appId: "durable-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.from({
      key: "durable",
      name: "durable",
      type: "durable",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch(input, init) {
            const request = new Request(input, init);
            requests.push(request);
            if (new URL(request.url).pathname.endsWith("/realtime/calls")) {
              return new Response("v=answer", {
                headers: { "x-nanocodex-realtime-location": "/realtime/calls/rtc_connect" },
              });
            }
            if (new URL(request.url).pathname.endsWith("/realtime/ticket")) {
              return Response.json({ ticket: "one-use-ticket" });
            }
            return Response.json({ agent_id: agentId, session_id: agentId });
          },
          async request() { throw new Error("control-plane request was unexpected"); },
        };
      },
    }),
  });
  client._setSessionToken("grant-session-test");
  const connection = connectionFromWire(testConnectionWire({
    agentId,
    sessionId,
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent", "agent.output.final", "chatgpt"],
  }));
  const agent = await client.agent.create({ connection });

  assert.equal(agent.id, agentId);
  assert.equal(agent.agentId, agentId);
  assert.equal(agent.sessionId, sessionId);
  assert.equal(agent.type, "connect");
  assert.equal(requests.length, 0);
  const voice = Voice.create(agent);
  assert.equal(voice.getSnapshot().status, "idle");
  await voice.destroy();
  const voiceTransport = managedBrowserVoiceTransport(agent);
  const providerCall = JSON.stringify({
    sdp: "v=offer",
    session: { delegation: { type: "client" } },
  });
  const voiceSessionId = "019fc927-b280-7aa7-8445-1b9996ad2fb1";
  const call = await voiceTransport.call(JSON.stringify({
    call_body: providerCall,
    managed_agent_id: agentId,
    openai_alpha: "quicksilver=v2",
    realtime_session_id: voiceSessionId,
    session_id: voiceSessionId,
    thread_id: voiceSessionId,
  }));
  assert.equal(await call.text(), "v=answer");
  assert.equal(new URL(requests[0].url).pathname, `/v1/grants/${connection.grant.id}/agents/${agentId}/realtime/calls`);
  assert.equal(requests[0].headers.get("authorization"), "Bearer grant-session-test");
  assert.equal(requests[0].headers.get("x-nanocodex-voice-session-id"), voiceSessionId);
  assert.equal(await requests[0].text(), providerCall);
  const sideband = await voiceTransport.sidebandUrl("rtc_connect");
  assert.equal(sideband.protocol, "wss:");
  assert.equal(sideband.searchParams.get("call_id"), "rtc_connect");
  assert.equal(sideband.searchParams.get("ticket"), "one-use-ticket");
  assert.equal(sideband.searchParams.get("voice_session_id"), voiceSessionId);
  assert.equal(new URL(requests[1].url).pathname, `/v1/grants/${connection.grant.id}/agents/${agentId}/realtime/ticket`);
  assert.equal(requests[1].headers.get("authorization"), "Bearer grant-session-test");
  assert.deepEqual(await requests[1].json(), {
    call_id: "rtc_connect",
    voice_session_id: voiceSessionId,
  });
  await assert.rejects(
    client.agent.create({ connection, sessionId: "browser-local" }),
    /do not accept app-local sessionId/,
  );
});

test("Connect sends managed reads as POST so browsers retain exact app-origin admission", async () => {
  const requests = [];
  const agentId = "019fc927-b280-79a7-8445-1b9996ad2fb0";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const client = Client.create({
    appId: "history-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.from({
      key: "history",
      name: "history",
      type: "history",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch(input, init) {
            const request = new Request(input, init);
            requests.push(request);
            return Response.json({ data: [], has_more: false, latest_cursor: "0" });
          },
          async request() { throw new Error("control-plane request was unexpected"); },
        };
      },
    }),
  });
  client._setSessionToken("grant-session-test");
  const connection = connectionFromWire(testConnectionWire({
    agentId,
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent", "agent.output.final", "agent.history.read", "chatgpt"],
  }));
  const agent = await client.agent.create({ connection });
  assert.equal((await agent.events.page({ limit: 1 })).latestCursor, "0");
  assert.equal(requests.length, 1);
  assert.equal(requests[0].method, "POST");
  assert.equal(
    new URL(requests[0].url).pathname,
    `/v1/grants/${connection.grant.id}/agents/${agentId}/events/history`,
  );
  assert.equal(requests[0].headers.get("authorization"), "Bearer grant-session-test");
});

test("Connect POST-tunnels only the complete managed read surface", () => {
  for (const path of ["", "/events", "/events/history", "/turns/turn-1"]) {
    assert.equal(isManagedReadPath(path), true, path);
  }
  for (const path of [
    "/turns",
    "/turns/turn-1/cancel",
    "/turns/turn-1/steer",
    "/durability",
    "/realtime/calls",
  ]) {
    assert.equal(isManagedReadPath(path), false, path);
  }
});

test("ConnectAgent publishes app tools with only signed hosted MCPs over the ticketed tool host", async () => {
  const firstMcpId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const secondMcpId = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210_-abcde";
  const accountWideMcpId = "0123456789abcdefghijklmnopqrstuvwxyz_-ABCDE";
  const agentId = "019fc927-b280-79a7-8445-1b9996ad2fb0";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const requests = [];
  const sockets = [];
  const initialAttachment = new AbortController();
  let appToolDisposals = 0;
  const appTools = {
    app_echo: {
      description: "Echo an app-owned value.",
      parameters: {
        type: "object",
        properties: { value: { type: "string" } },
        required: ["value"],
        additionalProperties: false,
      },
      handler: ({ value }) => ({ value: `app:${value}` }),
      dispose() { appToolDisposals += 1; },
    },
  };
  const client = Client.create({
    appId: "grant-mcp-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.from({
      key: "grant-mcp",
      name: "grant-mcp",
      type: "grant-mcp",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch(input, init) {
            const request = new Request(input, init);
            requests.push(request);
            const path = new URL(request.url).pathname;
            if (path.endsWith("/tool-host/ticket")) {
              return Response.json({ ticket: "one-use-tool-ticket", expires_in: 30 });
            }
            const connectionId = path.split("/").at(-1);
            if (path.includes("/mcp/")
              && (connectionId === firstMcpId || connectionId === secondMcpId)) {
              const message = await request.clone().json();
              if (message.method === "initialize") {
                return Response.json({
                  jsonrpc: "2.0",
                  id: message.id,
                  result: {
                    protocolVersion: message.params.protocolVersion,
                    capabilities: { tools: {} },
                    serverInfo: { name: connectionId, version: "1.0.0" },
                  },
                });
              }
              if (message.method === "tools/list") {
                return Response.json({
                  jsonrpc: "2.0",
                  id: message.id,
                  result: {
                    tools: [{
                      name: connectionId === firstMcpId ? "search_issues" : "read_document",
                      description: `Tool from ${connectionId}`,
                      inputSchema: { type: "object", properties: {} },
                    }],
                  },
                });
              }
              if (message.method === "notifications/initialized") {
                return new Response(null, { status: 202 });
              }
            }
            return Response.json({ error: { message: "unexpected request" } }, { status: 404 });
          },
          async request() { throw new Error("control-plane request was unexpected"); },
        };
      },
    }),
  });
  client._setSessionToken("grant-session-test");
  const connection = connectionFromWire(testConnectionWire({
    agentId,
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: [
      "nanocodex.agent",
      "agent.output.final",
      "chatgpt",
      `mcp:${firstMcpId}`,
      `mcp:${secondMcpId}`,
    ],
    mcpConnections: [
      { id: firstMcpId, name: "Issue tracker" },
      { id: secondMcpId, name: "Documents" },
    ],
    appToolCatalogDigest: await appToolDigest(appTools),
  }));
  const OriginalWebSocket = globalThis.WebSocket;
  globalThis.WebSocket = class {
    readyState = 1;
    frames = [];
    listeners = new Map();

    constructor(url, protocols) {
      this.url = String(url);
      this.protocols = protocols;
      sockets.push(this);
    }

    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? [];
      listeners.push(listener);
      this.listeners.set(type, listeners);
    }

    send(encoded) {
      const frame = JSON.parse(encoded);
      this.frames.push(frame);
      if (frame.type === "drain") queueMicrotask(() => this.receive({ type: "draining" }));
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
  };

  let agent;
  try {
    let createSettled = false;
    const creating = client.agent.create({
      connection,
      signal: initialAttachment.signal,
      tools: appTools,
    });
    void creating.then(
      () => { createSettled = true; },
      () => { createSettled = true; },
    );
    await waitForConnect(() => sockets[0]?.frames.some(({ type }) => type === "catalog"));
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(createSettled, false);
    sockets[0].receive({ type: "ready" });
    agent = await creating;
    initialAttachment.abort(new Error("creation signal is detached after readiness"));
    assert.equal(sockets[0].readyState, 1);
    const socketUrl = new URL(sockets[0].url);
    assert.equal(socketUrl.pathname, `/v1/grants/${connection.grant.id}/agents/${agentId}/tool-host`);
    assert.equal(socketUrl.searchParams.get("ticket"), "one-use-tool-ticket");
    assert.equal(socketUrl.searchParams.has("authorization"), false);
    assert.equal(sockets[0].protocols, undefined);
    assert.equal(JSON.stringify({ url: sockets[0].url, protocols: sockets[0].protocols })
      .includes("grant-session-test"), false);

    const catalog = sockets[0].frames.find(({ type }) => type === "catalog").tools;
    assert.deepEqual(catalog.map(({ provider, remote_name }) => [provider, remote_name]), [
      ["javascript", "app_echo"],
      [`mcp:${firstMcpId}`, "search_issues"],
      [`mcp:${secondMcpId}`, "read_document"],
    ]);
    assert.equal(catalog.some(({ provider }) => provider === `mcp:${accountWideMcpId}`), false);

    sockets[0].receive({
      type: "call",
      session_id: "session:app-tool",
      call_id: "call:app-echo",
      model: "gpt-5.6-sol",
      name: "app_echo",
      input: { value: "hello" },
      output_token_budget: 10_000,
      output_byte_budget: 128 * 1024,
      deadline_at: Date.now() + 30_000,
    });
    await waitForConnect(() => sockets[0].frames.some(({ type, call_id: callId }) => (
      type === "result" && callId === "call:app-echo"
    )));
    assert.deepEqual(
      sockets[0].frames.find(({ type, call_id: callId }) => (
        type === "result" && callId === "call:app-echo"
      )),
      {
        type: "result",
        call_id: "call:app-echo",
        outcome: {
          status: "completed",
          output: {
            output: '{"value":"app:hello"}',
            success: true,
            structured_result: { value: "app:hello" },
            metadata: null,
            process_trace: null,
          },
        },
      },
    );
    sockets[0].receive({ type: "ack", call_id: "call:app-echo" });

    const mcpRequests = requests.filter((request) => new URL(request.url).pathname.includes("/mcp/"));
    assert.deepEqual([...new Set(mcpRequests.map((request) => new URL(request.url).pathname))], [
      `/v1/grants/${connection.grant.id}/mcp/${firstMcpId}`,
      `/v1/grants/${connection.grant.id}/mcp/${secondMcpId}`,
    ]);
    assert.equal(mcpRequests.every((request) => (
      request.headers.get("authorization") === "Bearer grant-session-test"
    )), true);
    assert.equal(requests.some((request) => request.url.includes(accountWideMcpId)), false);
    const ticketRequest = requests.find((request) => new URL(request.url).pathname.endsWith("/tool-host/ticket"));
    assert.equal(ticketRequest.method, "POST");
    assert.equal(ticketRequest.headers.get("authorization"), "Bearer grant-session-test");
  } finally {
    await agent?.session.shutdown();
    globalThis.WebSocket = OriginalWebSocket;
  }
  assert.equal(sockets[0].closed.code, 1000);
  assert.equal(appToolDisposals, 1);
});

test("ConnectAgent creation rejects a terminal tool attachment policy rejection", async () => {
  let appToolDisposals = 0;
  const tools = {
    app_echo: {
      description: "Echo.",
      handler: ({ value }) => value,
      dispose() { appToolDisposals += 1; },
    },
  };
  const fixture = await localToolAttachmentFixture("rejected-tool-host-workspace", undefined, tools);

  try {
    const creating = fixture.client.agent.create({
      connection: fixture.connection,
      tools,
    });
    const rejected = assert.rejects(
      creating,
      /tool attachment rejected: signed grant rejected catalog/,
    );
    await waitForConnect(() => fixture.sockets[0]?.frames.some(({ type }) => type === "catalog"));
    fixture.sockets[0].close(1008, "signed grant rejected catalog");
    await rejected;
  } finally {
    fixture.restore();
  }
  assert.equal(appToolDisposals, 1);
});

test("ConnectAgent creation aborts while waiting for initial tool attachment readiness", async () => {
  const controller = new AbortController();
  const reason = new Error("caller stopped waiting for tool readiness");
  let appToolDisposals = 0;
  const tools = {
    app_echo: {
      description: "Echo.",
      handler: ({ value }) => value,
      dispose() { appToolDisposals += 1; },
    },
  };
  const fixture = await localToolAttachmentFixture("aborted-tool-host-workspace", undefined, tools);
  try {
    const creating = fixture.client.agent.create({
      connection: fixture.connection,
      signal: controller.signal,
      tools,
    });
    const rejected = assert.rejects(creating, (error) => error === reason);
    await waitForConnect(() => fixture.sockets[0]?.frames.some(({ type }) => type === "catalog"));
    controller.abort(reason);
    await rejected;
    assert.equal(fixture.sockets[0].closed.code, 1000);
  } finally {
    fixture.restore();
  }
  assert.equal(appToolDisposals, 1);
});

for (const status of [401, 403, 409]) {
  test(`ConnectAgent creation rejects terminal tool-host ticket status ${status}`, async () => {
    const tools = { app_echo: { description: "Echo.", handler: ({ value }) => value } };
    const fixture = await localToolAttachmentFixture(`terminal-ticket-${status}`, {
      status,
      body: { error: { message: `terminal ticket ${status}` } },
    }, tools);
    try {
      await assert.rejects(
        fixture.client.agent.create({
          connection: fixture.connection,
          tools,
        }),
        new RegExp(`tool attachment rejected: terminal ticket ${status}`),
      );
      assert.equal(fixture.ticketRequests(), 1);
      assert.equal(fixture.sockets.length, 0);
    } finally {
      fixture.restore();
    }
  });
}

test("ConnectAgent with an empty signed MCP list creates no MCP runtime or tool-host socket", async () => {
  const agentId = "019fc927-b280-79a7-8445-1b9996ad2fb0";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  let fetches = 0;
  let sockets = 0;
  const client = Client.create({
    appId: "empty-mcp-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.from({
      key: "empty-mcp",
      name: "empty-mcp",
      type: "empty-mcp",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch() {
            fetches += 1;
            throw new Error("empty MCP grants must not fetch during Agent creation");
          },
        };
      },
    }),
  });
  client._setSessionToken("grant-session-test");
  const connection = connectionFromWire(testConnectionWire({
    agentId,
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent", "agent.output.final", "chatgpt"],
    mcpConnections: [],
  }));
  const OriginalWebSocket = globalThis.WebSocket;
  globalThis.WebSocket = class {
    constructor() { sockets += 1; }
  };
  try {
    const agent = await client.agent.create({ connection });
    await agent.session.shutdown();
  } finally {
    globalThis.WebSocket = OriginalWebSocket;
  }
  assert.equal(fetches, 0);
  assert.equal(sockets, 0);
});

test("Connect binds normalized cloud accounts into auth resources and the connection request", async () => {
  const requests = [];
  const fetches = [];
  const walletRequests = [];
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const client = Client.create({
    appId: "connector-workspace",
    appOrigin: "https://consumer.example",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        walletRequests.push(request);
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: {
              auth: { approval_id: "approval-test" },
              keyAuthorization: {
                address: keyId,
                keyId,
                keyType: "p256",
                chainId: 4217n,
                expiry,
                witness: `0x${"22".repeat(32)}`,
              },
              personalSign: { keyAuthorization: "0x1234" },
            },
          }],
        };
      },
    },
    transport: Transport.from({
      key: "capture",
      name: "capture",
      type: "capture",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch(input, init) {
            const request = new Request(input, init);
            fetches.push(request);
            return Response.json({ ok: true });
          },
          async request(request) {
            requests.push(request);
            return testConnectionWire({
              conversationId: "0f5f2ab8-2585-4d7c-9403-0de76f55ad18",
              expiry,
              keyId,
              capabilities: [
              "nanocodex.agent",
              "agent.output.final",
              "agent.output.actions",
              "agent.history.read",
              "agent.trace.read",
              "github",
              "gdrive",
              "x",
              "slack:TA",
              "slack:TB",
              ],
            });
          },
        };
      },
    }),
  });

  const connection = await client.connection.connect({
    conversationId: "0f5f2ab8-2585-4d7c-9403-0de76f55ad18",
    capabilities: {
      auth: {
        challenge: "https://connect.example/v1/connect/auth/challenge",
        verify: "https://connect.example/v1/connect/auth",
        logout: "https://connect.example/v1/connect/auth/logout",
        resources: [
          "urn:example:configured",
          "urn:nanocodex:app:connector-workspace",
          "urn:nanocodex:connector:github",
        ],
      },
      cloudAccounts: {
        github: true,
        gmail: false,
        gdrive: true,
        x: true,
        slack: true,
        chatgpt: "true",
        unknown: true,
      },
      agent: {
        finalMessages: false,
        actionSummaries: false,
        conversationHistory: false,
        rawTraces: true,
      },
    },
  });

  assert.deepEqual(walletRequests, [{
    method: "wallet_connect",
    params: [{
      chainId: "0x1079",
      capabilities: {
        auth: {
          challenge: "https://connect.example/v1/connect/auth/challenge",
          logout: "https://connect.example/v1/connect/auth/logout",
          resources: [
            "urn:example:configured",
            "urn:nanocodex:app:connector-workspace",
            "urn:nanocodex:origin:https%3A%2F%2Fconsumer.example",
            "urn:nanocodex:connectors:github,gdrive,x,slack",
            "urn:nanocodex:agent:visibility:reply,actions,history,traces",
            "urn:nanocodex:agent:conversation:0f5f2ab8-2585-4d7c-9403-0de76f55ad18",
          ],
        },
      },
    }],
  }]);
  assert.deepEqual(requests[0].body.requested_connectors, ["github", "gdrive", "x", "slack"]);
  assert.equal("agent" in requests[0].body, false);
  assert.equal("visibility" in requests[0].body, false);
  assert.equal(requests[0].body.approval_id, "approval-test");
  assert.equal(requests[0].headers, undefined);
  assert.deepEqual(connection.grant.connectors, ["github", "gdrive", "x", "slack:TA", "slack:TB"]);
  assert.deepEqual(connection.grant.visibility, {
    finalMessages: true,
    actionSummaries: true,
    conversationHistory: true,
    rawTraces: true,
  });
  assert.equal(connection.grant.conversationId, "0f5f2ab8-2585-4d7c-9403-0de76f55ad18");
  assert.equal("credentials" in connection.grant, false);
  assert.equal("account" in connection.grant, false);
  assert.equal(connection.mpp.balanceStatus, "ready");

  const refreshed = await client.mpp.getBalance({ grantId: connection.grant.id });
  assert.equal(requests[1].path, `/v1/grants/${connection.grant.id}/mpp/balance`);
  assert.equal(refreshed.mpp.balanceStatus, "ready");

  await client.fetch("/v1/agent/account-info", { headers: { accept: "application/json" } });
  assert.equal(fetches[0].url, "https://connect.example/v1/agent/account-info");
  assert.equal(fetches[0].headers.get("authorization"), "Bearer grant-session-test");
  const captured = client._captureSession();
  client._setSessionToken("replacement-grant-session");
  await captured.fetch("/v1/egress");
  assert.equal(fetches[1].headers.get("authorization"), "Bearer grant-session-test");
  assert.equal(fetches[1].headers.get("authorization")?.includes("replacement"), false);
  await assert.rejects(
    Promise.resolve().then(() => client.fetch("https://evil.example/steal")),
    /restricted to its configured API origin/,
  );
});

test("Connect signs and grants exact pre-registered MCP connections without forwarding host secrets", async () => {
  const mcpId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const walletRequests = [];
  const requests = [];
  const client = Client.create({
    appId: "mcp-workspace",
    appOrigin: "https://consumer.example",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        walletRequests.push(request);
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: {
              auth: { approval_id: "approval-mcp" },
              keyAuthorization: {
                address: keyId,
                keyId,
                keyType: "p256",
                chainId: 4217n,
                expiry,
                witness: `0x${"22".repeat(32)}`,
              },
              personalSign: { keyAuthorization: "0x1234" },
            },
          }],
        };
      },
    },
    session: false,
    transport: Transport.from({
      key: "mcp-capture",
      name: "mcp-capture",
      type: "mcp-capture",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async request(request) {
            requests.push(request);
            return testConnectionWire({
              expiry,
              keyId,
              capabilities: [
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "chatgpt",
                `mcp:${mcpId}`,
              ],
              mcpConnections: [{ id: mcpId, name: "Linear workspace" }],
            });
          },
        };
      },
    }),
  });

  const connection = await client.connection.connect({
    capabilities: {
      auth: { resources: ["urn:nanocodex:mcp:https://untrusted.example/mcp"] },
      cloudAccounts: { chatgpt: true },
    },
    focusMcpConnectionId: mcpId,
    mcpConnections: [{
      id: mcpId,
      name: "Linear workspace",
      endpoint: "https://mcp.linear.app/mcp",
      token: "provider-secret",
    }],
    permission: "agent.run",
  });

  assert.deepEqual(walletRequests[0].context, {
    focusMcpConnection: mcpId,
    requestedMcpConnections: [{
      id: mcpId,
      name: "Linear workspace",
      status: "authorization_required",
    }],
  });
  const resources = walletRequests[0].params[0].capabilities.auth.resources;
  assert.equal(resources.includes(`urn:nanocodex:mcp:${mcpId}`), true);
  assert.equal(resources.includes(`urn:nanocodex:mcp-focus:${mcpId}`), true);
  assert.equal(resources.some((resource) => resource.includes("untrusted.example")), false);
  assert.deepEqual(requests[0].body.requested_mcp_connections, [mcpId]);
  assert.deepEqual(connection.grant.mcpConnections, [{ id: mcpId, name: "Linear workspace" }]);
  const captured = JSON.stringify(
    { walletRequests, requests },
    (_key, value) => typeof value === "bigint" ? value.toString() : value,
  );
  assert.equal(captured.includes("provider-secret"), false);
  assert.equal(captured.includes("mcp.linear.app"), false);
  assert.throws(() => connectionFromWire(testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent", `mcp:${mcpId}`],
    mcpConnections: [{
      id: mcpId,
      name: "Linear workspace",
      endpoint: "https://mcp.linear.app/mcp",
    }],
  })), /private or unknown fields/);
  await assert.rejects(
    client.connection.connect({ mcpConnections: [{ id: `${mcpId}x`, name: "Invalid" }] }),
    /opaque 43-character IDs/,
  );
  assert.equal(walletRequests.length, 1);
});

test("Connect grants ChatGPT and an exact local tool catalog without delegated key authority", async () => {
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const walletRequests = [];
  const requests = [];
  const tools = [{
    name: "cleanup",
    description: "Preview or apply a declarative cleanup recipe to the selected browser tab.",
    parameters: {
      type: "object",
      properties: { recipe: { type: "object" } },
      required: ["recipe"],
      additionalProperties: false,
    },
    handler() {},
  }];
  const appToolCatalogDigest = await appToolDigest(tools);
  const client = Client.create({
    appId: "nanocodex-chrome",
    appOrigin: "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
    auth: { resources: ["urn:nanocodex:agent:run"] },
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        walletRequests.push(request);
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: { auth: { approval_id: "approval-hosted" } },
          }],
        };
      },
    },
    session: false,
    transport: Transport.from({
      key: "hosted-connect",
      name: "hosted-connect",
      type: "hosted-connect",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async request(request) {
            requests.push(request);
            return testConnectionWire({
              expiry,
              capabilities: [
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "agent.history.read",
                "agent.trace.read",
                "chatgpt",
              ],
              appToolCatalogDigest,
              authorizationMode: "hosted",
            });
          },
        };
      },
    }),
  });

  const connection = await client.connection.connect({
    authorization: "hosted",
    capabilities: {
      cloudAccounts: { chatgpt: true },
      agent: {
        finalMessages: true,
        actionSummaries: true,
        conversationHistory: true,
        rawTraces: true,
      },
    },
    permission: "agent.run",
    tools,
  });

  const resources = walletRequests[0].params[0].capabilities.auth.resources;
  assert.equal("authorizeAccessKey" in walletRequests[0].params[0].capabilities, false);
  assert.deepEqual(resources, [
    "urn:nanocodex:agent:run",
    "urn:nanocodex:app:nanocodex-chrome",
    "urn:nanocodex:origin:chrome-extension%3A%2F%2Fabcdefghijklmnopabcdefghijklmnop",
    "urn:nanocodex:connectors:chatgpt",
    "urn:nanocodex:agent:visibility:reply,actions,history,traces",
    "urn:nanocodex:authorization:hosted",
    `urn:nanocodex:app-tool-catalog:sha256:${appToolCatalogDigest.slice(2)}`,
  ]);
  assert.deepEqual(requests[0].body, {
    app_id: "nanocodex-chrome",
    account_address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
    approval_id: "approval-hosted",
    authorization_mode: "hosted",
    permission: "agent.run",
    requested_connectors: ["chatgpt"],
    requested_app_tool_catalog_digest: appToolCatalogDigest,
  });
  assert.equal(connection.authorization, "hosted");
  assert.equal(connection.accessKey, undefined);
  assert.equal(connection.mpp, undefined);
  assert.deepEqual(connection.grant.connectors, ["chatgpt"]);
  assert.equal(connection.grant.appToolCatalogDigest, appToolCatalogDigest);
});

test("Connect rejects an initially substituted MCP grant without persisting its session", async () => {
  const requestedMcpId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const substitutedMcpId = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210_-abcde";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const storage = memoryStorage();
  const client = Client.create({
    appId: "substituted-mcp-workspace",
    dialog: Dialog.memory(),
    provider: testWalletProvider({ expiry, keyId }),
    session: storage,
    transport: Transport.from({
      key: "substituted-mcp",
      name: "substituted-mcp",
      type: "substituted-mcp",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async request() {
            return testConnectionWire({
              expiry,
              keyId,
              capabilities: [
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "chatgpt",
                `mcp:${substitutedMcpId}`,
              ],
              mcpConnections: [{ id: substitutedMcpId, name: "Substituted workspace" }],
            });
          },
        };
      },
    }),
  });

  await assert.rejects(client.connection.connect({
    capabilities: { cloudAccounts: { chatgpt: true } },
    mcpConnections: [{ id: requestedMcpId, name: "Requested workspace" }],
    permission: "agent.run",
  }), /outside the exact approved request/);
  assert.equal(client._hasSession(), false);
  assert.equal(storage.getItem("nanocodex:connect:substituted-mcp-workspace:session"), null);
});

test("Connect rejects contradictory MCP capability and metadata projections", () => {
  const mcpId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const substitutedMcpId = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210_-abcde";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";

  assert.throws(() => connectionFromWire(testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent", `mcp:${mcpId}`],
    mcpConnections: [],
  })), /MCP capabilities and metadata must match exactly/);
  assert.throws(() => connectionFromWire(testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent"],
    mcpConnections: [{ id: mcpId, name: "Unbound metadata" }],
  })), /MCP capabilities and metadata must match exactly/);
  assert.throws(() => connectionFromWire(testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent", `mcp:${mcpId}`],
    mcpConnections: [{ id: substitutedMcpId, name: "Substituted metadata" }],
  })), /MCP capabilities and metadata must match exactly/);
});

test("Connect requires the managed conversation identity in every connection projection", () => {
  const wire = testConnectionWire({
    expiry: Math.floor(Date.now() / 1_000) + 3_600,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent"],
  });
  delete wire.session_id;
  assert.throws(() => connectionFromWire(wire), /connection\.session_id must be a non-empty string/);
});

test("Connect keeps the hosted dialog open until the grant session is committed", async () => {
  const events = [];
  let releaseConnection;
  const connectionGate = new Promise((resolve) => { releaseConnection = resolve; });
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const dialog = Dialog.from({
    key: "lifecycle",
    name: "Lifecycle",
    type: "test",
    setup() {
      return {
        host: "https://connect.example/dialog",
        showWallet() { events.push("dialog:show"); },
        hideWallet() { events.push("dialog:hide"); },
      };
    },
  });
  const client = Client.create({
    appId: "lifecycle-workspace",
    dialog,
    provider: {
      async request() {
        events.push("wallet:approved");
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: {
              auth: { approval_id: "approval-lifecycle" },
              keyAuthorization: { address: keyId, keyId, keyType: "p256", expiry },
              personalSign: { keyAuthorization: "0x1234" },
            },
          }],
        };
      },
    },
    transport: Transport.from({
      key: "lifecycle",
      name: "Lifecycle",
      type: "test",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch() { throw new Error("fetch was unexpected"); },
          async request() {
            events.push("grant:start");
            await connectionGate;
            events.push("grant:committed");
            return testConnectionWire({ expiry, keyId, capabilities: ["nanocodex.agent"] });
          },
        };
      },
    }),
  });

  const connecting = client.connection.connect();
  while (!events.includes("grant:start")) await Promise.resolve();
  assert.deepEqual(events, ["dialog:show", "wallet:approved", "grant:start"]);
  releaseConnection();
  await connecting;
  assert.deepEqual(events, [
    "dialog:show",
    "wallet:approved",
    "grant:start",
    "grant:committed",
    "dialog:hide",
  ]);
});

test("Connect reselects a reusable access key when the passkey account changes", async () => {
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const initialAccount = "0x1111111111111111111111111111111111111111";
  const selectedAccount = "0x2222222222222222222222222222222222222222";
  const initialKey = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const selectedKey = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const requests = [];
  const walletRequests = [];
  const provider = {
    store: {
      getState() {
        return {
          accounts: [{ address: initialAccount }],
          activeAccount: 0,
          accessKeys: [
            { access: initialAccount, address: initialKey, chainId: 4217, expiry },
            { access: selectedAccount, address: selectedKey, chainId: 4217, expiry },
          ],
        };
      },
    },
    async request(request) {
      walletRequests.push(request);
      return {
        accounts: [{
          address: selectedAccount,
          capabilities: { auth: { approval_id: "approval-selected" } },
        }],
      };
    },
  };
  const client = Client.create({
    appId: "account-switch-workspace",
    dialog: Dialog.memory(),
    provider,
    transport: Transport.from({
      key: "account-switch",
      name: "account-switch",
      type: "account-switch",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch() {
            return Response.json({ ok: true });
          },
          async request(request) {
            requests.push(request);
            if (request.method === "GET") return { registered: true };
            return {
              ...testConnectionWire({ expiry, keyId: selectedKey, capabilities: ["nanocodex.agent"] }),
              account_address: selectedAccount,
            };
          },
        };
      },
    }),
  });

  await client.connection.connect();

  assert.equal("authorizeAccessKey" in walletRequests[0].params[0].capabilities, false);
  assert.deepEqual(
    requests.filter((request) => request.method === "GET").map((request) => request.path),
    [
      `/v1/access-keys/${initialAccount}/${initialKey}?app_id=account-switch-workspace`,
      `/v1/access-keys/${selectedAccount}/${selectedKey}?app_id=account-switch-workspace`,
    ],
  );
  assert.deepEqual(requests.at(-1).body.reuse_access_key, {
    key_id: selectedKey,
    expiry,
  });
});

test("Connect rejects an empty spending policy before opening the wallet", async () => {
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const client = Client.create({
    appId: "empty-policy-workspace",
    accessKey: { authorize: { expiry, limits: [], scopes: [] } },
    dialog: Dialog.memory(),
    provider: {
      async request() {
        assert.fail("the wallet must not receive an ambiguous empty spending policy");
      },
    },
    transport: Transport.mock(),
  });

  await assert.rejects(
    client.connection.connect(),
    /access-key limits must contain at least one explicit spending constraint/,
  );
});

test("Connect preserves explicit zero spending and empty call scopes for signing", async () => {
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const limits = [
    { token: "0x20c0000000000000000000006637932dE5413804", limit: 0n, period: 0 },
    { token: "0x20C000000000000000000000b9537d11c60E8b50", limit: 0n, period: 0 },
  ];
  let walletRequest;
  const client = Client.create({
    appId: "zero-policy-workspace",
    accessKey: { authorize: { expiry, limits, scopes: [] } },
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        walletRequest = request;
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: {
              auth: { approval_id: "approval-zero-policy" },
              keyAuthorization: {
                address: "0x1111111111111111111111111111111111111111",
                keyId: "0x1111111111111111111111111111111111111111",
                keyType: "p256",
                chainId: 4217n,
                expiry,
                limits,
                scopes: [],
                witness: `0x${"22".repeat(32)}`,
              },
              personalSign: { keyAuthorization: "0x1234" },
            },
          }],
        };
      },
    },
    transport: Transport.mock(),
  });

  await client.connection.connect();

  assert.deepEqual(walletRequest.params[0].capabilities.authorizeAccessKey, {
    expiry,
    limits: [
      { token: limits[0].token, limit: "0x0", period: 0 },
      { token: limits[1].token, limit: "0x0", period: 0 },
    ],
    scopes: [],
  });
});

test("Connect persists, validates, and clears an app-scoped grant session", async () => {
  const mcpId = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const storage = memoryStorage();
  const requests = [];
  const wire = testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent", "agent.output.final", "chatgpt", `mcp:${mcpId}`],
    mcpConnections: [{ id: mcpId, name: "Linear workspace" }],
  });
  let refreshedWire = wire;
  const transport = Transport.from({
    key: "session",
    name: "session",
    type: "session",
    setup() {
      return {
        baseUrl: "https://connect.example",
        async request(request) {
          requests.push(request);
          if (request.method === "POST" && request.path === "/v1/connections") return wire;
          if (request.method === "POST" && request.path === `/v1/grants/${wire.grant.id}/reconnect`) return refreshedWire;
          if (request.method === "POST" && request.path === "/v1/connections/disconnect") return undefined;
          throw new Error(`unexpected request ${request.method} ${request.path}`);
        },
      };
    },
  });
  const provider = {
    async request() {
      return {
        accounts: [{
          address: wire.account_address,
          capabilities: {
            auth: { approval_id: "approval-session" },
            keyAuthorization: {
              address: keyId,
              keyId,
              keyType: "p256",
              chainId: 4217n,
              expiry,
              witness: wire.access_key.witness,
            },
            personalSign: { keyAuthorization: "0x1234" },
          },
        }],
      };
    },
  };
  const first = Client.create({
    appId: "session-workspace",
    dialog: Dialog.memory(),
    provider,
    session: storage,
    transport,
  });
  const connected = await first.connection.connect({
    capabilities: {
      agent: { actionSummaries: false },
      cloudAccounts: { chatgpt: true },
    },
    mcpConnections: [{ id: mcpId, name: "Linear workspace" }],
    permission: "agent.run",
  });
  assert.equal(connected.grant.id, wire.grant.id);
  const { grant_token: _grantToken, ...connectionWire } = wire;
  assert.deepEqual(JSON.parse(storage.getItem("nanocodex:connect:session-workspace:session")), {
    grantId: wire.grant.id,
    token: wire.grant_token,
    connection: connectionWire,
  });

  const restoredClient = Client.create({
    appId: "session-workspace",
    dialog: Dialog.memory(),
    provider: {
      request() { throw new Error("wallet must not reopen"); },
    },
    session: storage,
    transport,
  });
  assert.equal(restoredClient._resumeConnection().agentId, wire.agent_id);
  const restored = await restoredClient.connection.reconnect();
  assert.equal(restored.grant.id, connected.grant.id);
  assert.deepEqual(restored.grant.mcpConnections, [{ id: mcpId, name: "Linear workspace" }]);
  assert.equal(requests.at(-1).method, "POST");
  assert.equal(requests.at(-1).path, `/v1/grants/${wire.grant.id}/reconnect`);
  assert.equal(requests.at(-1).headers.authorization, `Bearer ${wire.grant_token}`);

  const substitutedStorage = memoryStorage();
  substitutedStorage.setItem(
    "nanocodex:connect:session-workspace:session",
    storage.getItem("nanocodex:connect:session-workspace:session"),
  );
  const substitutedId = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210_-abcde";
  refreshedWire = testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent", "agent.output.final", "chatgpt", `mcp:${substitutedId}`],
    mcpConnections: [{ id: substitutedId, name: "Substituted workspace" }],
  });
  const substitutedClient = Client.create({
    appId: "session-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet must not reopen"); } },
    session: substitutedStorage,
    transport,
  });
  assert.equal(await substitutedClient.connection.reconnect(), undefined);
  assert.equal(substitutedClient._hasSession(), false);
  refreshedWire = wire;

  const mismatchedStorage = memoryStorage();
  mismatchedStorage.setItem(
    "nanocodex:connect:session-workspace:session",
    storage.getItem("nanocodex:connect:session-workspace:session"),
  );
  const mismatchedClient = Client.create({
    appId: "session-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet must not reopen"); } },
    session: mismatchedStorage,
    transport,
  });
  assert.equal(mismatchedClient._resumeConnection({
    capabilities: {
      agent: { finalMessages: true, actionSummaries: true },
      cloudAccounts: { github: true, chatgpt: true },
    },
    permission: "agent.run",
  }), undefined);

  const exactConnectorClient = Client.create({
    appId: "session-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet must not reopen"); } },
    session: mismatchedStorage,
    transport,
  });
  assert.equal(exactConnectorClient._resumeConnection({
    capabilities: {
      cloudAccounts: { github: true, gmail: true, gdrive: true, x: true, slack: true, chatgpt: true },
    },
    permission: "agent.run",
  }), undefined);
  const mismatched = await mismatchedClient.connection.reconnect({
    capabilities: {
      agent: { finalMessages: true, actionSummaries: true },
      cloudAccounts: { github: true, chatgpt: true },
    },
    permission: "agent.run",
  });
  assert.equal(mismatched, undefined);
  assert.equal(mismatchedClient._hasSession(), false);

  await restoredClient.connection.disconnect();
  assert.equal(storage.getItem("nanocodex:connect:session-workspace:session"), null);
  assert.equal(restoredClient._hasSession(), false);
});

test("Connect clears its local grant before remote revocation", async () => {
  const storage = memoryStorage();
  let client;
  const transport = Transport.from({
    key: "disconnect-failure",
    name: "disconnect-failure",
    type: "disconnect-failure",
    setup() {
      return {
        baseUrl: "https://connect.example",
        async request(request) {
          assert.equal(request.path, "/v1/connections/disconnect");
          assert.equal(client._hasSession(), false);
          throw new Error("control plane unavailable");
        },
      };
    },
  });
  client = Client.create({
    appId: "disconnect-failure-workspace",
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet must not be used"); } },
    session: storage,
    transport,
  });
  client._setSession({
    grantId: `0x${"78".repeat(32)}`,
    token: "grant-session",
  });

  await assert.rejects(client.connection.disconnect(), /control plane unavailable/);
  assert.equal(client._hasSession(), false);
  assert.equal(storage.getItem("nanocodex:connect:disconnect-failure-workspace:session"), null);
});

test("Connect account logout clears the local session without revoking the app grant", async () => {
  const storage = memoryStorage();
  const walletRequests = [];
  let providerResets = 0;
  const client = Client.create({
    appId: "logout-workspace",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        walletRequests.push(request);
      },
      async reset() {
        providerResets += 1;
      },
    },
    session: storage,
    transport: Transport.from({
      key: "logout",
      name: "logout",
      type: "logout",
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch() { throw new Error("unused"); },
          async request() { throw new Error("grant transport must not be called"); },
        };
      },
    }),
  });
  client._setSession({
    grantId: `0x${"12".repeat(32)}`,
    token: "grant-session",
  });

  await client.account.logout();
  assert.equal(client._hasSession(), false);
  assert.deepEqual(walletRequests, [{ method: "wallet_disconnect" }]);
  assert.equal(providerResets, 1);
});

test("Connect account logout clears the local session when wallet logout fails", async () => {
  const storage = memoryStorage();
  let providerResets = 0;
  const client = Client.create({
    appId: "failed-logout-workspace",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        assert.equal(request.method, "wallet_disconnect");
        throw new Error("account service unavailable");
      },
      async reset() {
        providerResets += 1;
      },
    },
    session: storage,
    transport: Transport.mock(),
  });
  client._setSession({
    grantId: `0x${"34".repeat(32)}`,
    token: "grant-session",
  });

  await assert.rejects(client.account.logout(), /account service unavailable/);
  assert.equal(client._hasSession(), false);
  assert.equal(providerResets, 1);
});

test("Connect account logout clears the local session before remote wallet cleanup", async () => {
  const storage = memoryStorage();
  let release;
  let started;
  const remoteStarted = new Promise((resolve) => { started = resolve; });
  const remoteCleanup = new Promise((resolve) => { release = resolve; });
  const client = Client.create({
    appId: "local-first-logout-workspace",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        assert.equal(request.method, "wallet_disconnect");
        started();
        await remoteCleanup;
      },
    },
    session: storage,
    transport: Transport.mock(),
  });
  client._setSession({
    grantId: `0x${"56".repeat(32)}`,
    token: "grant-session",
  });

  const logout = client.account.logout();
  await remoteStarted;
  assert.equal(client._hasSession(), false);
  release();
  await logout;
});

test("the mock Connect transport preserves X in requested connector permissions", async () => {
  const transport = Transport.mock({ appName: "Test Workspace" }).setup({ appId: "x-workspace" });
  const prepared = await transport.request({
    method: "POST",
    path: "/v1/connections/prepare",
    body: {
      permission: "agent.run",
      resources: ["urn:nanocodex:connector:x"],
    },
  });

  assert.deepEqual(prepared.permission.connectors.at(-1), {
    id: "x",
    name: "X",
    detail: "Use the connected X account through the grant",
  });
});

test("Nanocodex Connect signs one witness-bound access key and enforces its MPP permission", async () => {
  const expiry = Math.floor(Date.now() / 1_000) + 30 * 86_400;
  const keyId = "0x1111111111111111111111111111111111111111";
  const witness = `0x${"22".repeat(32)}`;
  const client = Client.create({
    appId: "test-workspace",
    dialog: Dialog.memory(),
    provider: {
      async request(request) {
        assert.equal(request.method, "wallet_connect");
        assert.deepEqual(request.params[0].capabilities.auth.resources, [
          "repositories",
          "model-entitlement",
          "urn:nanocodex:app:test-workspace",
          "urn:nanocodex:agent:visibility:reply,actions",
        ]);
        return {
          accounts: [{
            address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
            capabilities: {
              auth: { approval_id: "approval-test" },
              keyAuthorization: {
                address: keyId,
                keyId,
                keyType: "p256",
                chainId: 4217n,
                expiry,
                witness,
              },
              personalSign: { keyAuthorization: "0x1234" },
            },
          }],
        };
      },
    },
    transport: Transport.mock({ appName: "Test Workspace" }),
  });

  let connection = await Actions.connection.connect(client, {
    capabilities: {
      auth: { resources: [
        "repositories",
        "model-entitlement",
        "urn:nanocodex:agent:trace:read",
      ] },
    },
  });
  assert.equal(connection.grant.status, "active");
  assert.deepEqual(connection.grant.visibility, {
    finalMessages: true,
    actionSummaries: true,
    conversationHistory: false,
    rawTraces: false,
  });
  assert.equal(connection.accessKey.keyId, keyId);
  assert.equal(connection.accessKey.witness, witness);
  assert.equal(connection.accessKey.authorization, "0x1234");
  assert.equal(connection.mpp.balance, 0n);
  assert.match(connection.agentId, /^agent_/);

  const funding = client.machineUsd.fund({
    accountAddress: connection.accountAddress,
    grantId: connection.grant.id,
    usdAmountCents: 500,
  });
  const fundingRequest = await nextDialogRequest(client.dialog);
  assert.equal(fundingRequest.type, "machineUsdFund");
  assert.equal(fundingRequest.usdAmountCents, 500);
  client.dialog.respond({
    order: {
      id: "ord_test",
      status: "complete",
      usd_amount_cents: 500,
      machine_usd_amount_atomics: "5000000",
      issuance_transaction_hash: `0x${"33".repeat(32)}`,
    },
  });
  connection = (await funding).connection;
  assert.equal(connection.mpp.balance, 0n);

  await assert.rejects(
    client.mpp.charge({
      amount: 300_000n,
      grantId: connection.grant.id,
      origin: "https://models.example",
    }),
    (error) => error instanceof Errors.HttpError
      && error.code === "mpp_request_limit_exceeded"
      && error.status === 403,
  );

  const revoked = await client.grant.revoke({ grantId: connection.grant.id });
  assert.equal(revoked.status, "revoked");
});

test("recognized visibility capabilities do not receive the legacy output fallback", () => {
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  const connection = connectionFromWire(testConnectionWire({
    expiry,
    keyId,
    capabilities: ["nanocodex.agent", "agent.history.read"],
  }));

  assert.deepEqual(connection.grant.visibility, {
    finalMessages: false,
    actionSummaries: false,
    conversationHistory: true,
    rawTraces: false,
  });
});

test("durable conversation projections require an exact lowercase UUIDv4", () => {
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const keyId = "0x1111111111111111111111111111111111111111";
  assert.throws(() => connectionFromWire(testConnectionWire({
    capabilities: ["nanocodex.agent"],
    conversationId: "not-a-conversation",
    expiry,
    keyId,
  })), /conversation_id must be a lowercase UUIDv4/);
});

test("ConnectAgent projections hide terminal output outside the signed resources", async () => {
  assert.deepEqual(projectAgentObservations({
    finalMessages: false,
    actionSummaries: false,
  }, "secret final", ["tool.search"]), {
    finalMessage: "",
    capabilitiesUsed: [],
  });

});

async function nextDialogRequest(dialog) {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    const request = dialog.getRequest();
    if (request) return request;
    await Promise.resolve();
  }
  throw new Error("Nanocodex Connect did not open its dialog");
}

function testConnectionWire({
  expiry,
  keyId,
  capabilities,
  agentId = "agent_connectors",
  sessionId = "019fc927-b280-79a7-8445-1b9996ad2fb1",
  conversationId,
  mcpConnections = [],
  appToolCatalogDigest,
  authorizationMode = "access_key",
}) {
  return {
    grant_token: "grant-session-test",
    account_address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
    agent_id: agentId,
    session_id: sessionId,
    grant: {
      id: `0x${"33".repeat(32)}`,
      permission: "agent.run",
      status: "active",
      expires_at: expiry,
      capabilities,
      ...(conversationId === undefined ? {} : { conversation_id: conversationId }),
      mcp_connections: mcpConnections,
      ...(appToolCatalogDigest === undefined ? {} : {
        app_tool_catalog_digest: appToolCatalogDigest,
      }),
    },
    authorization_mode: authorizationMode,
    ...(authorizationMode === "hosted" ? {} : { access_key: {
      address: keyId,
      chain_id: "4217",
      key_id: keyId,
      key_type: "p256",
      limits: [],
      scopes: [],
      witness: `0x${"22".repeat(32)}`,
      expiry,
      authorization: "0x1234",
    } }),
    ...(authorizationMode === "hosted" ? {} : { mpp: {
      token: "0x20c0000000000000000000000000000000000001",
      symbol: "MACHUSD",
      balance_status: "ready",
      settlement_token: "0x20C000000000000000000000b9537d11c60E8b50",
      settlement_symbol: "USDC.e",
      settlement_balance_atomics: "0",
      limit_atomics: "10000000",
      max_per_request_atomics: "250000",
      period: 86_400,
      balance_atomics: "0",
      spent_atomics: "0",
    } }),
  };
}

function testWalletProvider({ expiry, keyId }) {
  return {
    async request() {
      return {
        accounts: [{
          address: "0x8ba1f109551bd432803012645ac136ddd64dba72",
          capabilities: {
            auth: { approval_id: "approval-test" },
            keyAuthorization: {
              address: keyId,
              keyId,
              keyType: "p256",
              chainId: 4217n,
              expiry,
              witness: `0x${"22".repeat(32)}`,
            },
            personalSign: { keyAuthorization: "0x1234" },
          },
        }],
      };
    },
  };
}

function memoryStorage() {
  const values = new Map();
  return {
    getItem(key) { return values.get(key) ?? null; },
    setItem(key, value) { values.set(key, String(value)); },
    removeItem(key) { values.delete(key); },
  };
}

async function localToolAttachmentFixture(appId, ticketFailure, tools) {
  const agentId = "019fc927-b280-79a7-8445-1b9996ad2fb0";
  const expiry = Math.floor(Date.now() / 1_000) + 3_600;
  const sockets = [];
  let ticketRequestCount = 0;
  const client = Client.create({
    appId,
    dialog: Dialog.memory(),
    provider: { request() { throw new Error("wallet should not be used"); } },
    transport: Transport.from({
      key: appId,
      name: appId,
      type: appId,
      setup() {
        return {
          baseUrl: "https://connect.example",
          async fetch(input, init) {
            const request = new Request(input, init);
            if (new URL(request.url).pathname.endsWith("/tool-host/ticket")) {
              ticketRequestCount += 1;
              if (ticketFailure) {
                return Response.json(ticketFailure.body, { status: ticketFailure.status });
              }
              return Response.json({ ticket: "local-tool-ticket", expires_in: 30 });
            }
            return Response.json({ error: { message: "unexpected request" } }, { status: 404 });
          },
        };
      },
    }),
  });
  client._setSessionToken("grant-session-test");
  const connection = connectionFromWire(testConnectionWire({
    agentId,
    expiry,
    keyId: "0x1111111111111111111111111111111111111111",
    capabilities: ["nanocodex.agent", "agent.output.final", "chatgpt"],
    mcpConnections: [],
    appToolCatalogDigest: await appToolDigest(tools),
  }));
  const OriginalWebSocket = globalThis.WebSocket;
  globalThis.WebSocket = class {
    readyState = 1;
    frames = [];
    listeners = new Map();

    constructor(url) {
      this.url = String(url);
      sockets.push(this);
    }

    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? [];
      listeners.push(listener);
      this.listeners.set(type, listeners);
    }

    send(encoded) { this.frames.push(JSON.parse(encoded)); }

    close(code, reason) {
      if (this.readyState === 3) return;
      this.readyState = 3;
      this.closed = { code, reason };
      for (const listener of this.listeners.get("close") ?? []) listener({ code, reason });
    }
  };
  return {
    client,
    connection,
    sockets,
    ticketRequests: () => ticketRequestCount,
    restore() { globalThis.WebSocket = OriginalWebSocket; },
  };
}

async function appToolDigest(tools) {
  return hostedToolCatalogDigest(hostedAppToolCatalog(tools));
}

async function waitForConnect(predicate, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("timed out waiting for Connect state");
    await new Promise((resolve) => setImmediate(resolve));
  }
}
