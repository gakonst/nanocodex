import assert from "node:assert/strict";
import test from "node:test";
import { routeManaged, type ManagedProxyEnv } from "./managedProxy.ts";
import { apiKeyDigest } from "nanocodex/cloudflare/managed-auth";

const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
const url = "https://nanocodex.example/v1/agents/live";
const request = (query = "", headers: Record<string, string> = {}) => new Request(url + query, {
  headers: { authorization: `Bearer ${token}`, upgrade: "websocket", ...headers },
});
const digest = await apiKeyDigest(request());
const record = { id: "k".repeat(12), prefix: `ncx_live_${"k".repeat(12)}`, label: "fixture", createdAt: 1, digest,
  userId: "11111111-1111-4111-8111-111111111111", organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", role: "writer", authorizationEpoch: 2,
  capabilities: ["agents:read", "agents:write", "tools:use"] };
function fixture(value: unknown = record) {
  const calls = { auth: 0, session: 0, fallback: 0 };
  let forwarded: Request | undefined, sessionId: string | undefined;
  const env: ManagedProxyEnv = {
    NANOCODEX_BACKEND: { fetch: async () => { calls.fallback++; return new Response("old"); } } as unknown as Fetcher,
    NANOCODEX_LIVE_API_KEYS: { getByName(name) { assert.equal(name, digest); return { resolveAuthorizedKey: async () => { calls.auth++; return value; } }; } },
    NANOCODEX_LIVE_SESSIONS: { getByName(name) { sessionId = name; return { fetch: async candidate => {
      calls.session++; forwarded = candidate;
      return new Response("fixture", { headers: { "x-nanocodex-prepare": "active-conversation" } });
    } }; } },
  };
  return { env, calls, forwarded: () => forwarded!, sessionId: () => sessionId! };
}
async function run(req: Request, env: ManagedProxyEnv) { return (await routeManaged(req, env, new URL(req.url)))!; }

test("native creation uses live key authority once and the existing internal route/settings/prepare contract", async () => {
  const f = fixture();
  const req = request("?model=gpt-6-sol&thinking=high&reasoning_mode=pro&fast_mode=true", {
    "x-nanocodex-prepare": "active-conversation", "x-nanocodex-create-session-id": "forged", "x-nanocodex-owner-id": "forged",
    "x-nanocodex-session-organization-id": "forged", "x-nanocodex-session-team-id": "forged",
    "x-nanocodex-authorization-epoch": "999", "x-nanocodex-capabilities": '["organization:write"]',
    "x-nanocodex-request-principal": "forged", "x-nanocodex-client-ingress-colo": "SFO", "x-nanocodex-worker-colo": "SFO",
    "x-nanocodex-access": "ignored-on-upgrade", "x-nanocodex-api-key-authorized": "1",
  });
  Object.defineProperty(req, "cf", { value: { colo: "FRA" } });
  const response = await run(req, f.env);
  assert.equal(response.status, 200); assert.equal(await response.text(), "fixture");
  assert.deepEqual(f.calls, { auth: 1, session: 1, fallback: 0 });
  const internal = f.forwarded(), target = new URL(internal.url);
  assert.equal(target.origin, "https://session.internal"); assert.equal(target.pathname, "/create-live");
  assert.deepEqual(Object.fromEntries(target.searchParams), { model: "gpt-6-sol", thinking: "high", reasoning_mode: "pro", fast_mode: "true", public_origin: "https://nanocodex.example" });
  assert.match(f.sessionId(), /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.equal(internal.headers.get("x-nanocodex-create-session-id"), f.sessionId());
  assert.equal(internal.headers.get("x-nanocodex-owner-id"), record.userId);
  assert.equal(internal.headers.get("x-nanocodex-session-organization-id"), record.organizationId);
  assert.equal(internal.headers.get("x-nanocodex-session-team-id"), record.teamId);
  assert.equal(internal.headers.get("x-nanocodex-authorization-epoch"), "2");
  assert.deepEqual(JSON.parse(internal.headers.get("x-nanocodex-capabilities")!), record.capabilities);
  assert.deepEqual(JSON.parse(internal.headers.get("x-nanocodex-request-principal")!), { kind: "api_key", user_id: record.userId });
  assert.equal(internal.headers.get("x-nanocodex-client-ingress-colo"), "FRA"); assert.equal(internal.headers.has("x-nanocodex-worker-colo"), false);
  assert.equal(internal.headers.get("x-nanocodex-prepare"), "active-conversation");
  assert.equal(response.headers.get("x-nanocodex-prepare"), "active-conversation");
  assert.ok(response.headers.get("x-nanocodex-request-id")); assert.match(response.headers.get("server-timing")!, /managed_auth/);
});

test("query and upgrade errors occur before auth or creation", async () => {
  for (const query of ["?model=gpt-6-astra&model=gpt-6-sol", "?unknown=1", "?thinking=bad", "?fast_mode=1", "?public_origin=https://evil", "?reasoning_mode=pro"]) {
    const f = fixture(); assert.equal((await run(request(query), f.env)).status, 400); assert.deepEqual(f.calls, { auth: 0, session: 0, fallback: 0 });
  }
  const f = fixture(), req = request(); req.headers.delete("upgrade");
  assert.equal((await run(req, f.env)).status, 426); assert.deepEqual(f.calls, { auth: 0, session: 0, fallback: 0 });
});

test("denied, malformed, wrong-digest and narrow keys never create or fall back", async () => {
  for (const value of [undefined, {}, { ...record, digest: "x".repeat(43) }, { ...record, authorizationEpoch: 0 }, { ...record, capabilities: ["agents:read", "forged"] }]) {
    const f = fixture(null); f.env.NANOCODEX_LIVE_API_KEYS = { getByName: () => ({ resolveAuthorizedKey: async () => value }) };
    assert.equal((await run(request(), f.env)).status, 401); assert.equal(f.calls.session, 0); assert.equal(f.calls.fallback, 0);
  }
  for (const capability of record.capabilities) {
    const f = fixture({ ...record, capabilities: record.capabilities.filter(c => c !== capability) });
    assert.equal((await run(request(), f.env)).status, 403); assert.equal(f.calls.session, 0); assert.equal(f.calls.fallback, 0);
  }
});

test("cookies, Connect, nonnative routes and missing bindings preserve the exact old request", async () => {
  for (const headers of [{ cookie: "nanocodex_account=synthetic" }, { cookie: "nc_perf_legacy_path=1" }, { "x-nanocodex-connect-user": record.userId }, { authorization: "Bearer malformed" }] as Record<string,string>[]) {
    const f = fixture(); assert.equal(await (await run(request("", headers), f.env)).text(), "old"); assert.deepEqual(f.calls, { auth: 0, session: 0, fallback: 1 });
  }
  for (const binding of ["NANOCODEX_LIVE_API_KEYS", "NANOCODEX_LIVE_SESSIONS"] as const) {
    const f = fixture(); delete f.env[binding]; await run(request(), f.env); assert.deepEqual(f.calls, { auth: 0, session: 0, fallback: 1 });
  }
  const f = fixture(); f.env.NANOCODEX_LIVE_API_KEYS = { getByName: () => ({}) }; await run(request(), f.env); assert.equal(f.calls.fallback, 1); assert.equal(f.calls.session, 0);
});

test("other methods and agent routes preserve the original request without touching direct namespaces", async () => {
  for (const [method, path] of [["POST", "/v1/agents/live"], ["GET", "/v1/agents"],
    ["GET", "/v1/agents/11111111-1111-4111-8111-111111111111/ws"], ["GET", "/v1/agents/live/extra"]]) {
    const req = new Request("https://nanocodex.example" + path, { method, headers: { authorization: `Bearer ${token}`, upgrade: "websocket" } });
    let fallback = 0;
    const inaccessible = { getByName() { assert.fail("ineligible request touched direct namespace"); } };
    const env: ManagedProxyEnv = { NANOCODEX_LIVE_API_KEYS: inaccessible, NANOCODEX_LIVE_SESSIONS: inaccessible,
      NANOCODEX_BACKEND: { fetch: async (candidate: Request) => { fallback++; assert.equal(candidate, req); return new Response("old"); } } as unknown as Fetcher };
    assert.equal(await (await run(req, env)).text(), "old"); assert.equal(fallback, 1);
  }
});

test("ambiguous dispatch/auth errors do not retry, fall back, or leak private error details", async () => {
  for (const stage of ["auth", "session"]) {
    const f = fixture();
    if (stage === "auth") f.env.NANOCODEX_LIVE_API_KEYS = { getByName: () => ({ resolveAuthorizedKey: async () => { f.calls.auth++; throw Error("private credential"); } }) };
    else f.env.NANOCODEX_LIVE_SESSIONS = { getByName: () => ({ fetch: async () => { f.calls.session++; throw Error("private session"); } }) };
    const response = await run(request(), f.env); assert.equal(response.status, 503); assert.deepEqual(await response.json(), { error: "managed_service_unavailable" });
    assert.deepEqual(f.calls, { auth: 1, session: stage === "session" ? 1 : 0, fallback: 0 });
  }
});

test("creation correlation is retained on completed and ambiguous dispatch without private inputs", async () => {
  for (const fails of [false, true]) {
    const f = fixture(), logs: Record<string, unknown>[] = [];
    if (fails) f.env.NANOCODEX_LIVE_SESSIONS = { getByName: () => ({ fetch: async () => { throw Error("private-dispatch-error"); } }) };
    const original = console.info;
    console.info = value => { logs.push(value); };
    try {
      const response = await run(request("", { "x-private-fixture": "private-header" }), f.env);
      const records = logs.filter(value => value.type === "managed.agent.live_created");
      assert.equal(records.length, 1);
      const created = records[0];
      assert.equal(created.auth_kind, "api_key"); assert.equal(created.route, "direct_live");
      assert.equal(created.thread_id, created.agent_id); assert.equal(created.outcome, "failure");
      assert.match(String(created.agent_id), /^[0-9a-f-]{36}$/); assert.match(String(created.request_id), /^[0-9a-f-]{36}$/);
      assert.equal(typeof created.create_ms, "number");
      if (!fails) { assert.equal(created.request_id, response.headers.get("x-nanocodex-request-id")); assert.equal(created.agent_id, f.sessionId()); }
      assert.equal(JSON.stringify(logs).includes(token), false); assert.equal(JSON.stringify(logs).includes("private-"), false);
    } finally { console.info = original; }
  }
});

test("inference-scoped credentials cannot touch direct namespaces even with ambient authority", async () => {
  const req = request("", { authorization: "Bearer nci_live_synthetic", cookie: "synthetic", "x-nanocodex-access": "synthetic" });
  const env = new Proxy({} as ManagedProxyEnv, { get() { assert.fail("scope rejection must precede bindings"); } });
  assert.equal((await run(req, env)).status, 403);
});

test("direct live auth disposes returned records before dispatch and denial", async () => {
  for (const valid of [true, false]) {
    let disposals = 0;
    const value = { ...record, digest: valid ? digest : "x".repeat(43),
      [Symbol.dispose]() { disposals++; } };
    const f = fixture(value);
    const result = await run(request(), f.env);
    assert.equal(disposals, 1);
    assert.equal(result.status, valid ? 200 : 401);
    assert.equal(f.calls.session, valid ? 1 : 0);
    if (valid) assert.deepEqual(JSON.parse(f.forwarded().headers.get("x-nanocodex-capabilities")!), record.capabilities);
    assert.equal(f.calls.fallback, 0);
  }
});
