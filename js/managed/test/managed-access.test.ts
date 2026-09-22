import { describe, expect, it, vi } from "vitest";
import { authenticate, type AccountAuthEnv, type Principal } from "../src/account-auth";
import { MANAGED_ACCESS_HEADER, managedAccessResponse, observeManagedAccess, readManagedAccess } from "../src/managed-access";

const env = { NANOCODEX_ACCESS_SECRET: "a-test-key-with-at-least-32-bytes-of-entropy" };
const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "22222222-2222-4222-8222-222222222222", teamId: "33333333-3333-4333-8333-333333333333",
  role: "writer", subjectId: "api_key:fixture", credentialId: "fixture", authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write"],
};
function request(path = "/v1/agents", token?: string, extra: Record<string, string> = {}) {
  return new Request(`https://managed.test${path}`, { headers: {
    authorization: "Bearer fixture-account-key", ...(token ? { [MANAGED_ACCESS_HEADER]: token } : {}), ...extra,
  } });
}
async function issued(source = request(), identity = principal) {
  await observeManagedAccess(source, env, identity, "live", 190);
  return (await managedAccessResponse(source, Response.json({ ok: true }), env)).headers.get(MANAGED_ACCESS_HEADER)!;
}

describe("short-lived managed request authority", () => {
  it("marks only failed token authentication as safe for a live-credential retry", async () => {
    for (const authenticated of [undefined, principal]) {
      const source = request("/v1/agents", "invalid");
      await observeManagedAccess(source, env, authenticated, "access", 0);
      const response = await managedAccessResponse(source, new Response(null, { status: 401 }), env);
      expect(response.headers.get("x-nanocodex-access-rejected")).toBe(authenticated ? null : "1");
    }
  });
  it("reuses the exact scoped principal with no account/membership RPC", async () => {
    const token = await issued();
    const getByName = vi.fn(() => { throw new Error("remote authority must not be read"); });
    const cached = await authenticate(request("/v1/agents/a/turns", token), {
      ...env, NANOCODEX_API_KEYS: { getByName }, NANOCODEX_USERS: { getByName }, NANOCODEX_ORGANIZATIONS: { getByName },
    } as unknown as AccountAuthEnv);
    expect(cached).toEqual(principal);
    expect(getByName).not.toHaveBeenCalled();
    expect(token).not.toContain("fixture-account-key");
  });
  it("cannot extend a grant by using it", async () => {
    const token = await issued();
    const next = request("/v1/agents/a", token);
    await observeManagedAccess(next, env, principal, "access", 0.2);
    const response = await managedAccessResponse(next, Response.json({ ok: true }), env);
    expect(response.headers.get(MANAGED_ACCESS_HEADER)).toBeNull();
    expect(await readManagedAccess(next, env, Date.now() + 120_001)).toBeUndefined();
  });
  it("rejects tampering, a different origin, a changed login/key, and key rotation", async () => {
    const token = await issued();
    expect(await readManagedAccess(request("/v1/agents", token + "x"), env)).toBeUndefined();
    expect(await readManagedAccess(new Request("https://other.test/v1/agents", { headers: request("/v1/agents", token).headers }), env)).toBeUndefined();
    expect(await readManagedAccess(request("/v1/agents", token, { authorization: "Bearer different" }), env)).toBeUndefined();
    expect(await readManagedAccess(request("/v1/agents", token), { NANOCODEX_ACCESS_SECRET: "different-test-key-at-least-thirty-two-bytes" })).toBeUndefined();
  });
  it("never accepts the snapshot for agent sockets, streams, or account administration", async () => {
    const token = await issued();
    for (const path of ["/v1/auth/logout", "/v1/api-keys", "/v1/agents/a/ws", "/v1/agents/a/events", "/v1/agents/a/tool-host"]) {
      expect(await readManagedAccess(request(path, token), env)).toBeUndefined();
    }
    expect(await readManagedAccess(request("/v1/agents/a", token, { upgrade: "websocket" }), env)).toBeUndefined();
    expect(await readManagedAccess(request("/v1/agents/a", token, { accept: "text/event-stream" }), env)).toBeUndefined();
  });
  it("reuses authority for screen discovery, ICE and viewer admission but not publication or renewal", async () => {
    const source = request("/v1/account/hands/screens");
    const token = await issued(source);
    expect(token).toMatch(/^ncx_access_v1\./);
    for (const next of [request("/v1/account/hands/screens", token),
      new Request(request("/v1/account/hands/ice", token), { method: "POST" }),
      request("/v1/account/hands/view?machine_id=mac&surface_id=display&generation=one", token, { upgrade: "websocket" })]) {
      expect(await readManagedAccess(next, env)).toEqual(principal);
      expect(await readManagedAccess(next, env, Date.now() + 120_001)).toBeUndefined();
    }
    for (const path of ["/v1/account/hands/host", "/v1/account/hands/renew", "/v1/account/tool-host", "/v1/account/vm-host"]) {
      expect(await readManagedAccess(request(path, token, { upgrade: "websocket" }), env)).toBeUndefined();
      expect(await readManagedAccess(new Request(request(path, token), { method: "POST" }), env)).toBeUndefined();
    }
    expect(await readManagedAccess(request("/v1/account/hands/view", token), env)).toBeUndefined();
    expect(await readManagedAccess(request("/v1/account/hands/ice", token), env)).toBeUndefined();
  });
  it("preserves the upgraded viewer socket while exposing its authentication timing", async () => {
    const source = request("/v1/account/hands/view", await issued(), { upgrade: "websocket" });
    await observeManagedAccess(source, env, principal, "access", 0.2);
    const pair = new WebSocketPair();
    const upgraded = new Response(null, { status: 101, webSocket: pair[0] });
    const result = await managedAccessResponse(source, upgraded, env);
    expect(result.status).toBe(101);
    expect(result.webSocket).toBe(pair[0]);
    expect(result.headers.get("server-timing")).toContain('desc="access"');
    expect(result.headers.has(MANAGED_ACCESS_HEADER)).toBe(false);
  });
  it("binds browser reuse to its current cookie while preserving CSRF identity", async () => {
    const browser = { ...principal, kind: "account_session" as const };
    const token = await issued(request("/v1/agents", undefined, { cookie: "nanocodex_account=session-a" }), browser);
    expect((await readManagedAccess(request("/v1/agents/a", token, { cookie: "nanocodex_account=session-a" }), env))?.kind).toBe("account_session");
    expect(await readManagedAccess(request("/v1/agents/a", token, { cookie: "nanocodex_account=session-b" }), env)).toBeUndefined();
    expect(await readManagedAccess(request("/v1/agents/a", token), env)).toBeUndefined();
    expect((await readManagedAccess(request("/v1/agents/a", token, { cookie: "nanocodex_account = session-a" }), env))?.kind).toBe("account_session");
    expect(await readManagedAccess(request("/v1/agents/a", token, { cookie: "nanocodex_account = session-b; nanocodex_account=session-a" }), env)).toBeUndefined();
  });
  it("rejects a cached API identity when higher-priority authority is added", async () => {
    const token = await issued();
    expect(await readManagedAccess(request("/v1/agents/a", token, { cookie: "nanocodex_account = session-a" }), env)).toBeUndefined();
    expect(await readManagedAccess(request("/v1/agents/a", token, { "x-nanocodex-connect-user": principal.userId }), env)).toBeUndefined();
  });
  it("binds internal Connect grants to all current forwarded restrictions", async () => {
    const headers = { "x-nanocodex-connect-user": principal.userId, "x-nanocodex-connect-grant-id": "grant-one", "x-nanocodex-connect-connectors": '["chatgpt"]' };
    const source = new Request("https://nanocodex.internal/v1/agents", { headers });
    const connect: Principal = { ...principal, kind: "connect_grant", connectGrant: { grantId: "grant-one", connectors: ["chatgpt"], mcpIds: [] } };
    const token = await issued(source, connect);
    expect(await readManagedAccess(new Request(source, { headers: { ...headers, [MANAGED_ACCESS_HEADER]: token } }), env)).toEqual(connect);
    expect(await readManagedAccess(new Request(source, { headers: { ...headers, [MANAGED_ACCESS_HEADER]: token, "x-nanocodex-connect-connectors": '["chatgpt","github"]' } }), env)).toBeUndefined();
  });
  it("does not issue tokens on errors or cacheable responses", async () => {
    for (const response of [new Response(null, { status: 403 }), new Response("preview", { headers: { "cache-control": "private, max-age=3600" } })]) {
      const source = request();
      await observeManagedAccess(source, env, principal, "live", 190);
      expect((await managedAccessResponse(source, response, env)).headers.get(MANAGED_ACCESS_HEADER)).toBeNull();
    }
  });
});
