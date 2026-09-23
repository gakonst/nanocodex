import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import { MANAGED_ACCESS_TTL_MS } from "../src/managed-access";
import { ACCOUNT_DISCOVERY_TTL_MS } from "../src/account-catalog";

const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:idle-discovery", credentialId: "idle-discovery",
  authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
};
const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;

async function fixture(run: (f: Awaited<ReturnType<typeof setup>>) => Promise<void>, idleTimeoutMs = 30_000) {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (instance, state) => {
    const f = await setup(instance, state, idleTimeoutMs);
    try { await run(f); }
    finally {
      f.logs.mockRestore();
      await state.storage.deleteAlarm();
    }
  });
}

async function setup(instance: DurableAgentSession, state: DurableObjectState, idleTimeoutMs?: number) {
  const counts = { catalog: 0, vault: 0, hands: 0, inference: 0, responses: 0, close: 0 };
  const stages: Record<string, unknown>[] = [];
  const logs = vi.spyOn(console, "info").mockImplementation((entry) => {
    if (entry && typeof entry === "object") stages.push(entry as Record<string, unknown>);
  });
  const behavior = {
    bind: async () => new Response(null, { status: 204 }),
    catalog: async () => Response.json({ connectors: {}, mcp_connections: [] }),
  };
  const sockets: ModelSocket[] = [];
  const sends: number[] = [];
  class ModelSocket extends EventTarget {
    readyState = 1;
    bufferedAmount = 0;
    constructor(readonly id: number) { super(); }
    accept() {}
    close() { counts.close++; this.readyState = 3; }
    send() {
      sends.push(this.id);
      queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
        type: "response.completed", response: {
          id: `routing-fixture-${sends.length}`, status: "completed", end_turn: true,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello" }] }],
          usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
        },
      }) })));
    }
  }
  const original = (instance as unknown as { env: Record<string, unknown> }).env;
  Object.defineProperty(instance, "env", { configurable: true, value: { ...original,
    NANOCODEX_THREAD_ROUTING: "true",
    ...(idleTimeoutMs === undefined ? {} : { AGENT_IDLE_TIMEOUT_MS: String(idleTimeoutMs) }),
    AI: { run: async () => { counts.inference++; return {}; } },
    NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      if (url.pathname.includes("/responses") && (init?.method ?? (input instanceof Request ? input.method : "GET")) === "POST") {
        counts.responses++;
        return new Response(`data: ${JSON.stringify({ type: "response.completed", response: {
          id: `routing-http-${counts.responses}`, status: "completed", end_turn: true,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello" }] }],
          usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
        } })}\n\n`, { headers: { "content-type": "text/event-stream" } });
      }
      if (url.pathname.includes("/responses")) {
        const socket = new ModelSocket(sockets.length); sockets.push(socket);
        return { status: 101, headers: new Headers(), webSocket: socket };
      }
      if (url.pathname.endsWith("/catalog")) { counts.catalog++; return behavior.catalog(); }
      if (url.pathname.endsWith("/credentials/vault")) { counts.vault++; return Response.json({ vault: [] }); }
      if (url.pathname.startsWith("/subjects/")) return behavior.bind();
      return Response.json({ connectors: {}, mcp_connections: [] });
    } },
    NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
    NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => {
      counts.hands++; return Response.json({ tools: [], machines: [] });
    } }) },
  } });
  const request = (path: string, body?: unknown, authority = principal) => {
    const headers = new Headers();
    if (path !== "/create") forwardPrincipalAssertions(headers, authority);
    return instance.fetch(new Request(`https://session.internal${path}`, {
      method: body === undefined && path === "/state" ? "GET" : "POST", headers,
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }));
  };
  const created = await request("/create", {
    session_id: "0198d3f0-8844-7000-8000-000000000092", owner_id: principal.userId,
    organization_id: principal.organizationId, team_id: principal.teamId, authorization_epoch: 1,
    public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS,
    configuration: {},
  });
  expect(created.status).toBe(200);
  const snapshot = async () => (await (await request("/state")).json()) as {
    agent_loaded: boolean; accepted_turns: number; completed_turns: number;
  };
  const prepare = async (authority = principal) => {
    const completed = stages.filter(e => e.stage === "conversation.prepare").length;
    expect((await request("/prepare", undefined, authority)).status).toBe(202);
    await vi.waitFor(() => expect(stages.filter(e => e.stage === "conversation.prepare").length).toBeGreaterThan(completed));
    // The stage completes just before the single-flight reservation is released.
    await new Promise(resolve => setTimeout(resolve, 0));
  };
  return { instance, state, request, prepare, snapshot, behavior, counts, sockets, sends, logs, stages };
}

describe("fixed-model idle discovery lifecycle", () => {
  it("opens one owned socket while catalog is blocked and coalesces preparation", () => fixture(async f => {
    const catalog = Promise.withResolvers<Response>();
    f.behavior.catalog = () => catalog.promise;
    try {
      expect((await f.request("/prepare")).status).toBe(202);
      await vi.waitFor(() => expect(f.sockets).toHaveLength(1));
      expect(f.counts.catalog).toBe(1);
      expect(f.sends).toEqual([]);
      expect(await f.snapshot()).toMatchObject({ agent_loaded: false });
      expect((await f.request("/prepare")).status).toBe(202);
      expect(f.counts.catalog).toBe(1);
    } finally { catalog.resolve(Response.json({ connectors: {}, mcp_connections: [] })); }
    await vi.waitFor(async () => expect(await f.snapshot()).toMatchObject({ agent_loaded: true }));
    expect(f.sockets).toHaveLength(1);
    expect(f.sends).toEqual([]);
  }), 20_000);

  it("retires the preparation socket before a model change joins blocked discovery", () => fixture(async f => {
    const catalog = Promise.withResolvers<Response>();
    f.behavior.catalog = () => catalog.promise.then(response => response.clone());
    let changed: Promise<Response> | undefined;
    try {
      expect((await f.request("/prepare")).status).toBe(202);
      await vi.waitFor(() => expect(f.sockets).toHaveLength(1));
      const headers = new Headers(); forwardPrincipalAssertions(headers, principal);
      changed = f.instance.fetch(new Request("https://session.internal/settings", {
        method: "PATCH", headers, body: JSON.stringify({ model: "gpt-6-luna" }),
      }));
      await vi.waitFor(() => expect(f.sockets[0].readyState).toBe(3));
      expect(f.sockets).toHaveLength(1);
      expect(f.sends).toEqual([]);
    } finally { catalog.resolve(Response.json({ connectors: {}, mcp_connections: [] })); }
    expect((await changed!).status).toBe(200);
    expect(f.sockets).toHaveLength(2);
    expect(await f.snapshot()).toMatchObject({ agent_loaded: true });
    expect(f.sends).toEqual([]);
  }), 20_000);

  it("installs a ready-agent catalog refresh when replacement construction joins it", () => fixture(async f => {
    await f.prepare();
    const catalog = Promise.withResolvers<Response>();
    f.behavior.catalog = () => catalog.promise.then(response => response.clone());
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + ACCOUNT_DISCOVERY_TTL_MS + 1_000);
    let changed: Promise<Response> | undefined;
    try {
      expect((await f.request("/prepare")).status).toBe(202);
      await vi.waitFor(() => expect(f.counts.catalog).toBe(2));
      const headers = new Headers(); forwardPrincipalAssertions(headers, principal);
      changed = f.instance.fetch(new Request("https://session.internal/settings", {
        method: "PATCH", headers, body: JSON.stringify({ model: "gpt-6-luna" }),
      }));
      await vi.waitFor(() => expect(f.sockets).toHaveLength(2));
    } finally {
      catalog.resolve(Response.json({ connectors: {}, mcp_connections: [
        { id: "a".repeat(43), name: "Fixture workspace", status: "connected" },
      ] }));
    }
    try {
      expect((await changed!).status).toBe(200);
      await vi.waitFor(() => expect(f.stages.filter(e => e.stage === "conversation.prepare")).toHaveLength(2));
      await new Promise(resolve => setTimeout(resolve, 0));
      await f.prepare();
      // Missing the joined discovery would cause the next ensure to detect a
      // changed catalog and unnecessarily retire/rebuild this new runtime.
      expect(f.sockets).toHaveLength(2);
      expect(f.sends).toEqual([]);
    } finally { clock.mockRestore(); }
  }), 20_000);

  it("reuses unexpired discovery after idle while closing and reopening the model socket", () => fixture(async f => {
    await f.prepare();
    expect((await f.request("/turns", { id: "before-idle", input: "Say hello" })).status).toBe(202);
    await vi.waitFor(async () => expect(await f.snapshot()).toMatchObject({ completed_turns: 1 }));
    const before = { ...f.counts };
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 36_000);
    try {
      await f.instance.alarm();
      expect(await f.snapshot()).toMatchObject({ agent_loaded: false });
      expect(f.sockets).toHaveLength(1);
      expect(f.sockets[0].readyState).toBe(3);
      await f.prepare();
      expect(f.counts).toMatchObject({ catalog: before.catalog, vault: before.vault, hands: before.hands, inference: 0 });
      expect(f.sockets).toHaveLength(2);
      expect((await f.request("/turns", { id: "after-idle", input: "Say hello again" })).status).toBe(202);
      await vi.waitFor(async () => expect(await f.snapshot()).toMatchObject({ completed_turns: 2 }));
      expect(f.sends).toEqual([0, 1]);
      expect(f.counts.catalog).toBe(before.catalog);
    } finally { clock.mockRestore(); }
  }), 20_000);

  it("expires retained discovery at its original deadline across repeated idle retirements", () => fixture(async f => {
    const startedAt = Date.now();
    await f.prepare();
    const clock = vi.spyOn(Date, "now").mockReturnValue(startedAt + 36_000);
    try {
      let handRefreshes = 1;
      let lastHandRefresh = 0;
      for (const elapsed of [36_000, 72_000, MANAGED_ACCESS_TTL_MS + 1_000, ACCOUNT_DISCOVERY_TTL_MS + 1_000]) {
        clock.mockReturnValue(startedAt + elapsed);
        await f.instance.alarm();
        expect(await f.snapshot()).toMatchObject({ agent_loaded: false });
        await f.prepare();
        const expected = elapsed > ACCOUNT_DISCOVERY_TTL_MS ? 2 : 1;
        if (elapsed - lastHandRefresh > MANAGED_ACCESS_TTL_MS) { handRefreshes++; lastHandRefresh = elapsed; }
        expect(f.counts).toMatchObject({ catalog: expected, vault: expected, hands: handRefreshes });
      }
      expect(f.sockets).toHaveLength(5);
      expect(f.sends).toEqual([]);
    } finally { clock.mockRestore(); }
  }), 20_000);

  it("retains the provider connection through a normal pause and retires it after five minutes", () => fixture(async f => {
    await f.prepare();
    expect((await f.request("/turns", { id: "initial-retained", input: "Say hello" })).status).toBe(202);
    await vi.waitFor(async () => expect(await f.snapshot()).toMatchObject({ completed_turns: 1 }));
    const startedAt = Date.now();
    const before = { ...f.counts };
    const clock = vi.spyOn(Date, "now").mockReturnValue(startedAt + 36_000);
    try {
      await f.instance.alarm();
      expect(await f.snapshot()).toMatchObject({ agent_loaded: true });
      expect(f.sockets).toHaveLength(1);
      expect(f.sockets[0].readyState).toBe(1);
      expect((await f.request("/turns", { id: "within-retention", input: "Say hello again" })).status).toBe(202);
      await vi.waitFor(async () => expect(await f.snapshot()).toMatchObject({ completed_turns: 2 }));
      expect(f.sends).toEqual([0, 0]);
      expect(f.counts).toMatchObject({ catalog: before.catalog, vault: before.vault, close: before.close });
      clock.mockReturnValue(startedAt + 36_000 + 301_000);
      await f.instance.alarm();
      expect(await f.snapshot()).toMatchObject({ agent_loaded: false });
      expect(f.sockets[0].readyState).toBe(3);
      await f.prepare();
      expect(f.sockets).toHaveLength(2);
      expect(f.counts).toMatchObject({ catalog: before.catalog, vault: before.vault });
    } finally { clock.mockRestore(); }
  }, 300_000), 20_000);

  it("keeps authorization projections and stale-epoch rejection after idle retirement", () => fixture(async f => {
    await f.prepare();
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 36_000);
    try {
      await f.instance.alarm();
      const before = { ...f.counts };
      expect((await f.request("/prepare", undefined, { ...principal, authorizationEpoch: 2 })).status).toBe(404);
      expect(f.counts).toEqual(before);
      const limited = { ...principal, capabilities: ["agents:write", "tools:use"] } as Principal;
      await f.prepare(limited);
      expect(f.counts).toMatchObject({ catalog: 1, vault: 1, inference: 0 });
    } finally { clock.mockRestore(); }
  }));

});
