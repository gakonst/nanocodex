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

function fixtureEnvironment() {
  const requests: Array<{ agentId: string; path: string; key: string | null; body: string }> = [];
  const turns = new Map<string, { input: unknown; receipt: Record<string, unknown> }>();
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
        if (path === "/credential-binding") return new Response(null, { status: 200 });
        if (path === "/credential-binding/bind") return new Response(null, { status: 204 });
        if (path === "/initialize") return new Response(null, { status: 200 });
        if (path === "/credential-binding/commit") return new Response(null, { status: 204 });
        if (path === "/turns") {
          const value = JSON.parse(body) as { id: string; input: unknown };
          const retained = turns.get(value.id);
          if (retained) {
            if (JSON.stringify(retained.input) !== JSON.stringify(value.input)) {
              return Response.json({ error: "idempotency_conflict" }, { status: 409 });
            }
            return Response.json(retained.receipt, { status: 200 });
          }
          const receipt = {
            turn_id: value.id,
            state: "accepted",
            input: value.input,
            accepted_cursor: "2",
            terminal_cursor: null,
            created_at: 1,
            accepted_at: 1,
            updated_at: 1,
            attempt_count: 0,
            retry_at: null,
          };
          turns.set(value.id, { input: value.input, receipt });
          return Response.json(receipt, { status: 202 });
        }
        return Response.json({ error: "not_found" }, { status: 404 });
      },
    }),
  };
  const memory = {
    getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }),
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
  it("converges creation and first-turn retries on stable server-owned identities", async () => {
    const { runtime, requests } = fixtureEnvironment();
    const body = {
      settings: {
        model: "gpt-5.6-luna",
        thinking: "low",
        reasoning_mode: "standard",
        fast_mode: false,
      },
      configuration: { tools: [], multi_agent: { enabled: false } },
      input: "Compute 17 * 19.",
    };
    const first = await run(runtime, body, "run:job-42");
    const replay = await run(runtime, body, "run:job-42");
    expect(first.status).toBe(201);
    expect(replay.status).toBe(200);
    const firstReceipt = await first.json<Record<string, unknown>>();
    const replayReceipt = await replay.json<Record<string, unknown>>();
    expect(replayReceipt).toEqual(firstReceipt);
    expect(firstReceipt.agent_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(firstReceipt.turn_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(firstReceipt.turn_idempotency_key).toMatch(/^agent-run:[0-9a-f]{64}$/);
    const turnRequests = requests.filter(({ path }) => path === "/turns");
    expect(turnRequests).toHaveLength(2);
    expect(new Set(turnRequests.map(({ agentId }) => agentId)).size).toBe(1);
    expect(new Set(turnRequests.map(({ key }) => key))).toEqual(
      new Set([firstReceipt.turn_idempotency_key]),
    );

    const conflict = await run(runtime, { ...body, input: "Changed prompt" }, "run:job-42");
    expect(conflict.status).toBe(409);
    expect(await conflict.json()).toMatchObject({ error: "idempotency_conflict" });
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
