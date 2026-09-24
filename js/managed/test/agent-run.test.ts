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

describe("combined managed agent creation", () => {
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
