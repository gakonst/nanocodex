import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { Principal } from "../src/account-auth";
import worker, { type Env } from "../src/index";

const principal: Principal = {
  kind: "api_key",
  userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner",
  subjectId: "user:11111111-1111-4111-8111-111111111111",
  credentialId: "test",
  authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"],
};

function fixtureEnvironment(createStatus = 200) {
  const requests: Array<{ agentId: string; path: string; key: string | null; body: string }> = [];
  const sessions = {
    idFromName: () => ({ toString: () => "a".repeat(64) }),
    getByName: (agentId: string) => ({
      fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const request = new Request(input, init);
        const path = new URL(request.url).pathname;
        const body = await request.text();
        requests.push({
          agentId,
          path,
          key: request.headers.get("idempotency-key"),
          body,
        });
        if (path === "/create") return Response.json({ prepare_ms: 1, initialize_ms: 1, commit_ms: 1 }, { status: createStatus });
        return Response.json({ error: "not_found" }, { status: 404 });
      },
    }),
  };
  const memory = {
    getByName: () => { throw new Error("Creation must not initialize memory eagerly"); },
  };
  return {
    requests,
    runtime: {
      ...(env as unknown as Env),
      NANOCODEX_SESSIONS: sessions,
      NANOCODEX_MEMORY: memory,
    } as unknown as Env,
  };
}

function run(runtime: Env, body: unknown, key?: string, actor = principal) {
  return worker.fetch(new Request("https://nanocodex.example/v1/agent-runs", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(key === undefined ? {} : { "idempotency-key": key }),
    },
    body: JSON.stringify(body),
  }), runtime, createExecutionContext(), actor);
}

describe("first-activation probe", () => {
  it("requires the admin API key before allocating either Durable Object ID", async () => {
    let allocations = 0;
    const runtime = { ...env, NANOCODEX_ADMIN_USER_ID: "other-admin",
      NANOCODEX_SESSIONS: { idFromName: () => { allocations++; return "named"; },
        newUniqueId: () => { allocations++; return "unique"; } } } as unknown as Env;
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents/activation-probe", {
      method: "POST", headers: { "x-nanocodex-probe-kind": "unique" },
    }), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(404);
    expect(allocations).toBe(0);
  });

  it("uses a fresh API-key DO as a same-Worker control without storing credentials", async () => {
    const runtime = { ...env, NANOCODEX_ADMIN_USER_ID: principal.userId,
      NANOCODEX_API_KEYS: {
        newUniqueId: () => "key-unique",
        get: (id: string) => {
          expect(id).toBe("key-unique");
          return { activationProbe: async () => Date.now() };
        },
      },
    } as unknown as Env;
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents/activation-probe", {
      method: "POST", headers: { "x-nanocodex-probe-kind": "key-unique" },
    }), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ kind: "key-unique", dispatch_ms: expect.any(Number) });
  });

  it.each(["named", "unique"])("times a fresh %s ID without exposing it", async (kind) => {
    const ids: string[] = [];
    const runtime = { ...env, NANOCODEX_ADMIN_USER_ID: principal.userId,
      NANOCODEX_SESSIONS: {
        idFromName: () => { ids.push("named"); return "named"; },
        newUniqueId: () => { ids.push("unique"); return "unique"; },
        get: (id: string) => {
          expect(id).toBe(kind);
          return { activationProbe: async () => ({
            constructor_entered_at_ms: Date.now(), constructor_ready_at_ms: Date.now(),
            constructor_ms: 2, constructor_base_ms: 1, handler_entered_at_ms: Date.now(),
          }) };
        },
      },
    } as unknown as Env;
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents/activation-probe", {
      method: "POST", headers: { "x-nanocodex-probe-kind": kind },
    }), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(200);
    expect(ids).toEqual([kind]);
    expect(await response.json()).toMatchObject({ kind, constructor_ms: 2, constructor_base_ms: 1 });
  });
});

describe("combined managed agent creation", () => {
  it("publishes create and session timing without leaking internal timestamps", async () => {
    const { runtime } = fixtureEnvironment();
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ settings: { model: "gpt-6-astra", thinking: "low",
        reasoning_mode: "standard", fast_mode: false } }),
    }), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(201);
    expect(response.headers.get("server-timing")).toContain("managed_session_create;dur=");
    expect(response.headers.get("server-timing")).toContain("managed_create;dur=");
    expect(response.headers.get("server-timing")).not.toContain("managed_session_pre_handler");
    expect(response.headers.get("server-timing")).not.toContain("managed_session_attach");
    expect(await response.json()).not.toHaveProperty("handler_entered_at_ms");
  });

  it.each([false, true])("retains only keyed preparations after exhausted creation retries (keyed=%s)", async (keyed) => {
    const { runtime, requests } = fixtureEnvironment(503);
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents", {
      method: "POST",
      headers: { "content-type": "application/json", ...(keyed ? { "idempotency-key": "create:retry" } : {}) },
      body: JSON.stringify({ settings: { model: "gpt-6-astra", thinking: "low", reasoning_mode: "standard", fast_mode: false } }),
    }), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(503);
    expect(requests.filter(({ path }) => path === "/create")).toHaveLength(5);
    expect(new Set(requests.map(({ agentId }) => agentId)).size).toBe(1);
    expect(requests.filter(({ path }) => path === "/session")).toHaveLength(keyed ? 0 : 1);
  });

  it("rejects invalid requests and authority before creating a session", async () => {
    const { runtime, requests } = fixtureEnvironment();
    expect((await run(runtime, { input: "hello" })).status).toBe(400);
    expect((await run(runtime, { input: "" }, "run:empty")).status).toBe(400);
    expect((await run(runtime, { input: "hello", unsupported: true }, "run:bad")).status)
      .toBe(400);
    expect((await run(runtime, { input: "hello" }, "run:forbidden", {
      ...principal,
      capabilities: ["agents:write"],
    })).status).toBe(403);
    expect(requests).toEqual([]);
  });
});
