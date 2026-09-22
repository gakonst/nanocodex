import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS, parseCompleteAgentSettings } from "../src/agent-settings";
import { parseConfiguration } from "../src/agent-configuration";
import { resolveThreadRoute, routingPolicySchema } from "../src/thread-model-routing";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";

const glm = { model: "@cf/zai-org/glm-5.3", thinking: "medium", reasoning_mode: "standard", fast_mode: false } as const;
const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:routing-fixture", credentialId: "routing-test",
  authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
};
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const request = (path: string, method: string, body?: unknown) => new Request(`https://session.internal${path}`, {
  method, ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { "content-type": "application/json" } }),
});
async function fixture(run: (instance: DurableAgentSession, state: DurableObjectState) => Promise<void>) {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (instance, state) => {
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES (1,?,?,?,?,1,'https://nanocodex.example','managed',?)`,
    "0198d3f0-8844-7000-8000-000000000092", principal.userId, principal.organizationId, principal.teamId, Date.now());
    await run(instance, state);
  });
}

function routingRequest(body: BodyInit | null = null): Request {
  const headers = new Headers({ "content-type": "application/json" });
  forwardPrincipalAssertions(headers, principal);
  return new Request("https://session.internal/routing", { method: "POST", headers, body });
}
async function withRouting(instance: DurableAgentSession, run: () => Promise<void>) {
  const runtime = (instance as unknown as { env: Record<string, unknown> }).env;
  const previous = { NANOCODEX_THREAD_ROUTING: runtime.NANOCODEX_THREAD_ROUTING, AI: runtime.AI };
  Object.assign(runtime, { NANOCODEX_THREAD_ROUTING: "true", AI: { run() { throw Error("enabling must not infer"); } } });
  try { await run(); } finally { Object.assign(runtime, previous); }
}

describe("managed routing admission", () => {
  it("enables after real creation and speculative runtime preparation without sending a message", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (instance, state) => {
      const original = (instance as unknown as { env: Record<string, unknown> }).env;
      let closed = 0, sends = 0;
      class ModelSocket extends EventTarget {
        readyState = 1;
        accept() {}
        close() { closed++; this.readyState = 3; }
        send() { sends++; }
      }
      Object.defineProperty(instance, "env", { configurable: true, value: { ...original,
        NANOCODEX_THREAD_ROUTING: "true", AI: { run() { throw Error("enabling must not infer"); } },
        NANOCODEX: { fetch: async (input: RequestInfo, init?: RequestInit) => {
          const url = typeof input === "string" ? input : input instanceof Request ? input.url : String(input);
          if (url.includes("/responses")) return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
          return Response.json({ connectors: {}, mcp_connections: [] });
        } },
        NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
        NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => new Response(null, { status: 503 }) }) },
      } });
      const created = await instance.fetch(request("/create", "POST", {
        session_id: "0198d3f0-8844-7000-8000-000000000092", owner_id: principal.userId,
        organization_id: principal.organizationId, team_id: principal.teamId, authorization_epoch: 1,
        public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS, configuration: { tools: [], environment: { network: { access: "disabled" } } },
      }));
      expect(created.status).toBe(200);
      const headers = new Headers(); forwardPrincipalAssertions(headers, principal);
      expect((await instance.fetch(new Request("https://session.internal/prepare", { method: "POST", headers }))).status).toBe(202);
      await vi.waitFor(() => {
        expect(state.storage.sql.exec("SELECT name FROM sqlite_master WHERE name = 'nanocodex_cloudflare_agent'").toArray()).toHaveLength(1);
        expect(state.storage.sql.exec("SELECT * FROM nanocodex_cloudflare_agent").toArray()).toHaveLength(1);
      });
      const response = await instance.fetch(routingRequest());
      expect(await response.json()).toMatchObject({ enabled: true });
      expect(response.status).toBe(200);
      expect(sends).toBe(0);
      expect(closed).toBeGreaterThan(0);
      expect(state.storage.sql.exec<{ accepted_turns: number }>("SELECT accepted_turns FROM session_state").one().accepted_turns).toBe(0);
      await state.storage.deleteAlarm();
    });
  }, 20_000);

  it("enables explicitly on an existing empty session, preserves policy, and locks settings", () => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      state.storage.sql.exec("INSERT INTO managed_configuration VALUES (1, ?)", JSON.stringify({ instructions: "Keep this", settings: DEFAULT_AGENT_SETTINGS }));
      const before = state.storage.sql.exec("SELECT * FROM managed_agent_settings").one();
      const response = await instance.fetch(routingRequest());
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({ enabled: true, model_routing: { strategy: "direct" }, settings: { model: before.model, thinking: before.thinking, reasoning_mode: before.reasoning_mode, fast_mode: !!before.fast_mode } });
      const configuration = await (await instance.fetch(request("/configuration", "GET"))).json();
      expect(configuration).toMatchObject({ instructions: "Keep this", model_routing: { strategy: "direct" } });
      expect(configuration).not.toHaveProperty("settings");
      expect(state.storage.sql.exec("SELECT * FROM managed_agent_settings").one()).toEqual(before);
      expect(state.storage.sql.exec("SELECT * FROM managed_thread_route").toArray()).toEqual([]);
      expect((await instance.fetch(request("/settings", "PATCH", { thinking: "high" }))).status).toBe(409);
      state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1");
      expect((await instance.fetch(routingRequest("{}"))).status).toBe(200);
    });
  }));

  it.each([
    ["kimi-k3", "low", 2], ["kimi-k3", "high", 2], ["mimo-v2.6-pro", "medium", 2],
    ["@cf/zai-org/glm-5.3", "high", 3],
  ])("manually constrains %s/%s while leaving its provider choice to first input", (model, thinking, count) => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      const response = await instance.fetch(routingRequest(JSON.stringify({ model, thinking })));
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({ enabled: true, automatic: false, settings: { model, thinking } });
      const configuration = await (await instance.fetch(request("/configuration", "GET"))).json() as { model_routing: { candidates: string[] } };
      expect(configuration.model_routing.candidates).toHaveLength(count as number);
      expect(configuration.model_routing.candidates.every(id => id.endsWith(`:${thinking}`))).toBe(true);
      expect(await (await instance.fetch(request("/state", "GET"))).json()).toMatchObject({ model_routing_enabled: true, model_routing_automatic: false, model_route: null });
      state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1");
      expect((await instance.fetch(routingRequest(JSON.stringify({ model: "gpt-6-astra", thinking: "low" })))).status).toBe(409);
      expect((await instance.fetch(routingRequest("{}"))).status).toBe(409);
      expect(await (await instance.fetch(request("/configuration", "GET"))).json()).toEqual(configuration);
    });
  }));

  it("switches manual to automatic and back before the first message without inferring", () => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      const choose = (model: string, thinking: string) => instance.fetch(routingRequest(JSON.stringify({ model, thinking })));
      expect((await choose("kimi-k3", "high")).status).toBe(200);
      expect((await instance.fetch(routingRequest("{}"))).status).toBe(200);
      expect(await (await instance.fetch(request("/state", "GET"))).json()).toMatchObject({ model_routing_automatic: true });
      const manual = await choose("gpt-6-astra", "max");
      expect(manual.status).toBe(200);
      expect(await manual.json()).toMatchObject({ enabled: false, automatic: false, settings: { model: "gpt-6-astra", thinking: "max" } });
      expect(await (await instance.fetch(request("/configuration", "GET"))).json()).not.toHaveProperty("model_routing");
      expect(state.storage.sql.exec("SELECT * FROM managed_thread_route").toArray()).toEqual([]);
      expect(state.storage.sql.exec<{ accepted_turns: number }>("SELECT accepted_turns FROM session_state").one().accepted_turns).toBe(0);
    });
  }));

  it.each([{ model: "kimi-k3", thinking: "medium" }, { model: "mimo-v2.6-pro", thinking: "max" }, { model: "unknown", thinking: "low" }])(
    "rejects invalid manual model/effort without changing retained selection %#", selection => fixture(async instance => {
      await withRouting(instance, async () => {
        expect((await instance.fetch(routingRequest("{}"))).status).toBe(200);
        const before = await (await instance.fetch(request("/configuration", "GET"))).json();
        expect((await instance.fetch(routingRequest(JSON.stringify(selection)))).status).toBe(400);
        expect(await (await instance.fetch(request("/configuration", "GET"))).json()).toEqual(before);
      });
    }));

  it("reports pending opt-in and the actual pinned provider/model for the footer", () => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      expect(await (await instance.fetch(request("/state", "GET"))).json()).toMatchObject({ model_routing_enabled: false, model_route: null });
      expect((await instance.fetch(routingRequest())).status).toBe(200);
      expect(await (await instance.fetch(request("/state", "GET"))).json()).toMatchObject({ model_routing_enabled: true, model_route: null });
      const route = await resolveThreadRoute({ run: async () => ({ answers: { candidate: { choice: "vercel:zai/glm-5.3:high", confidence: .99 }, family: { choice: "terminal", confidence: .99 } } }) },
        "Task", routingPolicySchema.parse({}), { openrouter: false, vercel: true });
      // The projection must use the saved route, independently of placeholder startup settings.
      state.storage.sql.exec("INSERT INTO managed_thread_route VALUES (1, ?)", JSON.stringify(route));
      expect(await (await instance.fetch(request("/state", "GET"))).json()).toMatchObject({ model_routing_enabled: true,
        model_route: { backend: route.backend, model: route.model, thinking: route.thinking } });
    });
  }));

  it.each(["accepted", "completed", "snapshot", "history"])("rejects %s before mutating configuration", kind => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      if (kind === "accepted") state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1");
      if (kind === "completed") state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1, completed_turns = 1");
      if (kind === "snapshot") {
        const table = "nanocodex_durable_states";
        state.storage.sql.exec(`CREATE TABLE IF NOT EXISTS ${table} (fixture TEXT)`);
        state.storage.sql.exec(`INSERT INTO ${table} VALUES ('retained')`);
      }
      if (kind === "history") {
        state.storage.sql.exec("CREATE TABLE nanocodex_cloudflare_events (event_json TEXT, created_at INTEGER)");
        state.storage.sql.exec("INSERT INTO nanocodex_cloudflare_events (event_json, created_at) VALUES ('{}', ?)", Date.now());
      }
      const response = await instance.fetch(routingRequest());
      expect(response.status).toBe(409);
      expect(await response.json()).toMatchObject({ error: "routing_requires_new_thread" });
      expect(await (await instance.fetch(request("/configuration", "GET"))).json()).toEqual({});
    });
  }));

  it("requires full account authority and deployment gates", () => fixture(async instance => {
    expect((await instance.fetch(request("/routing", "POST", {}))).status).toBe(403);
    const response = await instance.fetch(routingRequest());
    expect(response.status).toBe(503);
    expect(await response.json()).toMatchObject({ error: "routing_unavailable" });
  }));

  it.each(["null", "[]", "{", '{"enabled":false}', '{"model":"gpt-6-sol"}'])("rejects invalid enable body %s", body => fixture(async instance => {
    await withRouting(instance, async () => {
      expect((await instance.fetch(routingRequest(body))).status).toBe(400);
    });
  }));

  it("reserves routing before awaiting its body so a racing first turn cannot be accepted early", () => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      let release!: () => void;
      const body = new ReadableStream<Uint8Array>({ start(controller) {
        release = () => { controller.enqueue(new TextEncoder().encode("{}")); controller.close(); };
      } });
      const enabling = instance.fetch(routingRequest(body));
      const turn = instance.fetch(request("/turns", "POST", { id: "routing-race-first", input: "hello" }));
      await Promise.resolve(); await Promise.resolve();
      expect(state.storage.sql.exec<{ accepted_turns: number }>("SELECT accepted_turns FROM session_state").one().accepted_turns).toBe(0);
      release();
      expect((await enabling).status).toBe(200);
      expect((await turn).status).toBe(202);
      expect(state.storage.sql.exec<{ accepted_turns: number }>("SELECT accepted_turns FROM session_state").one().accepted_turns).toBe(1);
      expect(await (await instance.fetch(request("/configuration", "GET"))).json()).toHaveProperty("model_routing");
    });
  }));

  it("rechecks accepted history after an enable request body is delayed", () => fixture(async (instance, state) => {
    await withRouting(instance, async () => {
      let release!: () => void;
      const body = new ReadableStream<Uint8Array>({ start(controller) {
        release = () => { controller.enqueue(new TextEncoder().encode("{}")); controller.close(); };
      } });
      const enabling = instance.fetch(routingRequest(body));
      state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1");
      release();
      expect((await enabling).status).toBe(409);
      expect(await (await instance.fetch(request("/configuration", "GET"))).json()).toEqual({});
    });
  }));

  it("public routing forwards full principal and rejects Connect or insufficient authority", async () => {
    let forwarded = 0;
    const runtime = { ...env, NANOCODEX_SESSIONS: { getByName: () => ({ fetch: async (url: string, init: RequestInit) => {
      forwarded++;
      expect(url).toBe("https://session.internal/routing");
      expect(init.method).toBe("POST");
      expect(new Headers(init.headers).get("x-nanocodex-owner-id")).toBe(principal.userId);
      return Response.json({ enabled: true, settings: DEFAULT_AGENT_SETTINGS });
    } }) } } as unknown as Parameters<typeof worker.fetch>[1];
    const url = "https://nanocodex.example/v1/agents/0198d3f0-8844-7000-8000-000000000092/routing";
    for (const denied of [{ ...principal, capabilities: ["agents:write"] }, { ...principal, kind: "connect_grant" }]) {
      expect((await worker.fetch(new Request(url, { method: "POST" }), runtime, createExecutionContext(), denied as Principal)).status).toBe(403);
    }
    expect(forwarded).toBe(0);
    expect((await worker.fetch(new Request(url, { method: "POST" }), runtime, createExecutionContext(), principal)).status).toBe(200);
    expect(forwarded).toBe(1);
  });

  it.each([
    { name: "legacy empty request", body: "", routed: false },
    { name: "empty configuration", body: JSON.stringify({ configuration: {} }), routed: false },
    { name: "explicit model", body: JSON.stringify({ settings: { ...DEFAULT_AGENT_SETTINGS, model: "gpt-6-sol" } }), routed: false },
    { name: "unrelated tool policy", body: JSON.stringify({ configuration: { tools: ["exec_command"] } }), routed: false },
    { name: "explicit routing opt-in", body: JSON.stringify({ configuration: { model_routing: {} } }), routed: true },
  ])("API deployment preserves opt-in admission: $name", async ({ body, routed }) => {
    let admitted: any;
    const runtime = { ...env,
      NANOCODEX_THREAD_ROUTING: "true",
      // Even an obsolete deployment-wide auto flag must not enroll a client.
      NANOCODEX_AUTO_ROUTING: "true",
      AI: { run() { throw Error("admission must not call Jev"); } },
      NANOCODEX_USERS: { getByName() { return {}; } },
      NANOCODEX_SESSIONS: {
        idFromName: () => ({ toString: () => "fixture-session" }),
        getByName: () => ({ fetch: async (input: RequestInfo, init?: RequestInit) => {
          const request = new Request(input, init);
          expect(new URL(request.url).pathname).toBe("/create");
          admitted = await request.json();
          return Response.json({});
        } }),
      },
    } as unknown as Parameters<typeof worker.fetch>[1];
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents", { method: "POST", body }),
      runtime, createExecutionContext(), principal);
    expect(response.status).toBe(201);
    expect(!!admitted.configuration.model_routing).toBe(routed);
    expect(admitted.settings).toEqual(body.includes("gpt-6-sol") ? { ...DEFAULT_AGENT_SETTINGS, model: "gpt-6-sol" } : DEFAULT_AGENT_SETTINGS);
    if (routed) expect(admitted.configuration.model_routing.strategy).toBe("direct");
  });

  it.each([
    { settings: glm },
    { configuration: { settings: glm } },
  ])("rejects direct GLM HTTP creation before creating an agent: %j", async body => {
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents", {
      method: "POST", body: JSON.stringify(body),
    }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), principal);
    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ error: "invalid_request", message: expect.stringContaining("only through model_routing") });
  });

  it("preserves persisted GLM settings while rejecting direct settings mutations atomically", () => fixture(async (instance, state) => {
    expect(parseCompleteAgentSettings(glm)).toEqual(glm);
    const before = state.storage.sql.exec("SELECT * FROM managed_agent_settings").one();
    const response = await instance.fetch(request("/settings", "PATCH", glm));
    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ error: "invalid_request", message: expect.stringContaining("only through model_routing") });
    expect(state.storage.sql.exec("SELECT * FROM managed_agent_settings").one()).toEqual(before);
    expect(state.storage.sql.exec("SELECT * FROM managed_thread_route").toArray()).toEqual([]);
  }));

  it.each([false, true])("locks routed settings and portability (route already pinned: %s)", pinned => fixture(async (instance, state) => {
    state.storage.sql.exec("INSERT INTO managed_configuration VALUES (1, ?)", JSON.stringify(parseConfiguration({ model_routing: {} })));
    if (pinned) {
      const route = await resolveThreadRoute({ run: async () => ({ answers: { family: { choice: "terminal", confidence: .99 } } }) }, "Fix build", routingPolicySchema.parse({}));
      state.storage.sql.exec("INSERT INTO managed_thread_route VALUES (1, ?)", JSON.stringify(route));
      state.storage.sql.exec("UPDATE managed_agent_settings SET model = ?, thinking = ?, reasoning_mode = 'standard', fast_mode = 0", route.model, route.thinking);
    }
    const settings = state.storage.sql.exec("SELECT * FROM managed_agent_settings").one();
    const route = state.storage.sql.exec("SELECT * FROM managed_thread_route").toArray();
    for (const patch of [{ model: "gpt-6-sol" }, { thinking: "low" }]) {
      const response = await instance.fetch(request("/settings", "PATCH", patch));
      expect(response.status).toBe(409);
      expect(await response.json()).toMatchObject({ error: "settings_locked" });
    }
    for (const operation of ["export", "import"]) {
      const response = await instance.fetch(request(`/durability/${operation}`, "POST", {}));
      expect(response.status).toBe(409);
      expect(await response.json()).toMatchObject({ error: "routed_session_not_portable" });
    }
    expect(state.storage.sql.exec("SELECT * FROM managed_agent_settings").one()).toEqual(settings);
    expect(state.storage.sql.exec("SELECT * FROM managed_thread_route").toArray()).toEqual(route);
    // Export rejection must not fence the session as exported.
    const stillLocked = await instance.fetch(request("/settings", "PATCH", { thinking: "high" }));
    expect(await stillLocked.json()).toMatchObject({ error: "settings_locked" });
  }));
});
