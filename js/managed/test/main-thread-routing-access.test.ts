import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { Principal } from "../src/account-auth";
import worker, { type Env } from "../src/index";

const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", role: "owner",
  subjectId: "user:fixture", credentialId: "fixture", authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"],
};
const origin = "https://nanocodex.example";
function call(runtime: Env, path: string, method = "GET", actor = principal, requestOrigin?: string) {
  return worker.fetch(new Request(origin + path, {
    method, headers: requestOrigin ? { origin: requestOrigin } : {},
    ...(method === "PUT" ? { body: path === "/v1/main-thread" ? "{}" : JSON.stringify({ name: "Research" }) } : {}),
  }), runtime, createExecutionContext(), actor);
}

describe("public Main Thread routing authority", () => {
  it("rejects insufficient capabilities and Connect grants before accessing a registry or session", async () => {
    const unexpected = () => { throw new Error("unauthorized request reached account data"); };
    const runtime = { ...env, NANOCODEX_USERS: { getByName: unexpected }, NANOCODEX_SESSIONS: { getByName: unexpected } } as unknown as Env;
    const grant: NonNullable<Principal["connectGrant"]> = { grantId: `0x${"a".repeat(64)}` as const, connectors: ["chatgpt"], mcpIds: [] };
    for (const path of ["/v1/main-thread", "/v1/projects"]) {
      expect((await call(runtime, path, "GET", { ...principal, capabilities: [] })).status).toBe(403);
      expect((await call(runtime, path, "GET", { ...principal, kind: "connect_grant" })).status).toBe(403);
      expect((await call(runtime, path, "GET", { ...principal, connectGrant: grant })).status).toBe(403);
    }
    for (const path of ["/v1/main-thread", "/v1/projects/research"]) {
      expect((await call(runtime, path, "PUT", { ...principal, capabilities: ["agents:read"] })).status).toBe(403);
      expect((await call(runtime, path, "PUT", { ...principal, capabilities: ["agents:write"] })).status).toBe(403);
      expect((await call(runtime, path, "PUT", { ...principal, connectGrant: grant })).status).toBe(403);
    }
  });

  it("rejects cross-origin browser mutations before accessing account data", async () => {
    const runtime = { ...env, NANOCODEX_USERS: { getByName: () => { throw new Error("cross-origin mutation reached registry"); } } } as unknown as Env;
    for (const path of ["/v1/main-thread", "/v1/projects/research"]) {
      expect((await call(runtime, path, "PUT", { ...principal, kind: "account_session" }, "https://other.example")).status).toBe(403);
    }
  });

  it("selects the account and team exclusively from the authenticated principal", async () => {
    const requests: Array<{ owner: string; path: string; team: string | null }> = [];
    const runtime = { ...env, NANOCODEX_USERS: { getByName: (owner: string) => ({
      fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(new Request(input, init).url);
        requests.push({ owner, path: url.pathname, team: url.searchParams.get("team_id") });
        return Response.json({ data: [] });
      },
    }) } } as unknown as Env;
    const other = { ...principal, userId: "22222222-2222-4222-8222-222222222222", teamId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc" };
    for (const actor of [principal, other]) {
      expect((await call(runtime, "/v1/projects", "GET", actor)).status).toBe(200);
    }
    expect(requests).toEqual([principal, other].map(actor => ({ owner: actor.userId, path: "/projects", team: actor.teamId })));
    expect((await call(runtime, "/v1/projects?team_id=other-team")).status).toBe(400);
    expect(requests).toHaveLength(2);
  });
});
