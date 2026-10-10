import { Subagents } from "nanocodex/host";
import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { DEFAULT_OPENAI_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";

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
    // Fresh per fixture: a managed session ID is its runtime session ID, which
    // is unique per isolate across Durable Objects (one DO per session).
    session_id: "0198d3f0-8844-7000-8000-" + crypto.randomUUID().slice(-12), owner_id: principal.userId,
    organization_id: principal.organizationId, team_id: principal.teamId, authorization_epoch: 1,
    public_origin: "https://nanocodex.example", settings: DEFAULT_OPENAI_AGENT_SETTINGS,
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

// The directory and shutdown execute real Rust/WASM. Only the stored journal
// is seeded, simulating restart before managed child bindings are registered.
it.each(["interrupted", "completed", "failed", "closed", "unrecoverable", "explicit-shutdown", "recovery-exhausted"])(
  "idle alarm respects restored %s children without managed bindings",
  (scenario) => fixture(async f => {
    const kind = scenario === "explicit-shutdown" ? "interrupted" : scenario;
    // Runtime loss during every automatic resume must stop at the journaled budget.
    const exhausted = scenario === "recovery-exhausted";
    await f.prepare();
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 36_000);
    const realList = Subagents.list;
    let runtime: Parameters<typeof Subagents.list>[0] | undefined;
    const directory = vi.spyOn(Subagents, "list").mockImplementation(async (agent, options) => {
      runtime = agent;
      return realList(agent, options);
    });
    try {
      await f.instance.alarm();
      expect(await f.snapshot()).toMatchObject({ agent_loaded: false });
      const childSession = "0198d3f0-8844-7000-8000-000000000093";
      const status = exhausted ? { state: "running" }
        : kind === "completed" ? { state: kind, output: "done" }
        : kind === "failed" ? { state: kind, error: "fixture failure" }
        : { state: kind === "unrecoverable" ? "interrupted" : kind };
      const journal = { version: 1, agents: [{
        descriptor: { id: 73, session_id: childSession, role: "retained", task: "fixture", parent: null },
        status, output_schema: { type: "string" }, turn_in_flight: exhausted,
        ...(exhausted ? { resume_attempts: 3 } : {}),
        ...(kind === "unrecoverable" ? {} : { checkpoint: {
          session_id: childSession, model: "astra", thinking: "max",
          service_tier: "standard", conversation: null,
        } }),
      }] };
      const { state_id } = f.state.storage.sql.exec<{ state_id: string }>(
        "SELECT state_id FROM nanocodex_cloudflare_durability").one();
      f.state.storage.sql.exec("INSERT OR REPLACE INTO nanocodex_durable_states VALUES (?, '1', ?)",
        state_id + ":subagents", JSON.stringify(journal));
      runtime = undefined;
      await f.prepare();
      // Observe the real constructed handle without replacing runtime behavior.
      await vi.waitFor(() => expect(runtime).toBeDefined());
      const restored = runtime!;
      expect((await realList(restored, { includeCompleted: true })).agents)
        .toEqual(expect.arrayContaining([expect.objectContaining({ agent_id: 73, status: exhausted
          ? { state: "failed", error: expect.stringContaining("subagent recovery exhausted") } : status })]));
      expect((await realList(restored)).agents.map(a => a.agent_id))
        .toEqual(kind === "interrupted" ? [73] : []);
      if (scenario === "explicit-shutdown") {
        const headers = new Headers(); forwardPrincipalAssertions(headers, principal);
        const changed = await f.instance.fetch(new Request("https://session.internal/settings", {
          method: "PATCH", headers, body: JSON.stringify({ model: "gpt-6-luna" }),
        }));
        expect(changed.status).toBe(200);
        await f.prepare();
        expect(runtime).not.toBe(restored);
        await expect(realList(restored, { includeCompleted: true })).rejects.toThrow("disposed");
        return;
      }
      // A resting child (interrupted, completed, failed...) never pins the root:
      // the idle alarm retires the runtime and installs no further wakeup.
      clock.mockReturnValue(Date.now() + 36_000);
      await f.instance.alarm();
      expect(await f.snapshot()).toMatchObject({ agent_loaded: false });
      expect(await f.state.storage.getAlarm()).toBeNull();
      if (kind === "interrupted") {
        // The journal keeps the resting child; a later request restores it.
        runtime = undefined;
        await f.prepare();
        await vi.waitFor(() => expect(runtime).toBeDefined());
        expect(runtime).not.toBe(restored);
        expect((await realList(runtime!)).agents).toEqual([
          expect.objectContaining({ agent_id: 73, status: { state: "interrupted" }, can_message: true }),
        ]);
      }
    } finally { directory.mockRestore(); clock.mockRestore(); }
  }), 30_000,
);
