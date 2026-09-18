import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { Principal } from "../src/account-auth";
import worker, { type Env } from "../src/index";

const actor: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", role: "owner",
  subjectId: "user:fixture", credentialId: "fixture", authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"],
};
const origin = "https://nanocodex.example";
function call(runtime: Env, path: string, method = "GET", principal = actor, requestOrigin?: string) {
  return worker.fetch(new Request(origin + path, {
    method, headers: requestOrigin ? { origin: requestOrigin } : {},
    ...(method === "PUT" ? { body: path === "/v1/main-thread" ? "{}" : JSON.stringify({ name: "Research" }) } : {}),
  }), runtime, createExecutionContext(), principal);
}

describe("Main Thread HTTP authorization boundary", () => {
  it("rejects insufficient capabilities and Connect grants before touching account data", async () => {
    const unexpected = () => { throw new Error("unauthorized request reached account data"); };
    const runtime = { ...env, NANOCODEX_USERS: { getByName: unexpected }, NANOCODEX_SESSIONS: { getByName: unexpected } } as unknown as Env;
    const grant: NonNullable<Principal["connectGrant"]> = { grantId: `0x${"a".repeat(64)}` as const, connectors: ["chatgpt"], mcpIds: [] };
    for (const path of ["/v1/main-thread", "/v1/projects"]) {
      expect((await call(runtime, path, "GET", { ...actor, capabilities: [] })).status).toBe(403);
      expect((await call(runtime, path, "GET", { ...actor, kind: "connect_grant" })).status).toBe(403);
      expect((await call(runtime, path, "GET", { ...actor, connectGrant: grant })).status).toBe(403);
    }
    for (const path of ["/v1/main-thread", "/v1/projects/research"]) {
      expect((await call(runtime, path, "PUT", { ...actor, capabilities: ["agents:read"] })).status).toBe(403);
      expect((await call(runtime, path, "PUT", { ...actor, capabilities: ["agents:write"] })).status).toBe(403);
      expect((await call(runtime, path, "PUT", { ...actor, connectGrant: grant })).status).toBe(403);
    }
  });

  it("rejects cross-origin browser writes before touching account data", async () => {
    const runtime = { ...env, NANOCODEX_USERS: { getByName: () => { throw new Error("cross-origin write reached registry"); } } } as unknown as Env;
    for (const path of ["/v1/main-thread", "/v1/projects/research"]) {
      expect((await call(runtime, path, "PUT", { ...actor, kind: "account_session" }, "https://other.example")).status).toBe(403);
    }
  });

  it("takes owner and team from the authenticated principal, never the request query", async () => {
    const requests: Array<{ owner: string; path: string; team: string | null }> = [];
    const runtime = { ...env, NANOCODEX_USERS: { getByName: (owner: string) => ({
      fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(new Request(input, init).url);
        requests.push({ owner, path: url.pathname, team: url.searchParams.get("team_id") });
        return Response.json({ data: [] });
      },
    }) } } as unknown as Env;
    const other = { ...actor, userId: "22222222-2222-4222-8222-222222222222", teamId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc" };
    for (const principal of [actor, other]) expect((await call(runtime, "/v1/projects", "GET", principal)).status).toBe(200);
    expect(requests).toEqual([actor, other].map(principal => ({ owner: principal.userId, path: "/projects", team: principal.teamId })));
    expect((await call(runtime, "/v1/projects?team_id=other-team")).status).toBe(400);
    expect(requests).toHaveLength(2);
  });
});
