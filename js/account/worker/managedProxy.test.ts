import assert from "node:assert/strict";
import test from "node:test";

import { isManagedRoutePath, routeManaged } from "./managedProxy.ts";

test("the account Worker exposes only the exact managed wallet routes", () => {
  for (const path of [
    "/v1/wallet",
    "/v1/wallet/balance",
    "/v1/wallet/connect",
    "/v1/wallet/revoke-access-key",
  ]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }

  for (const path of [
    "/v1/wallet/",
    "/v1/wallet/export",
    "/v1/wallet/connect/extra",
    "/v1/wallet/revoke-access-key/extra",
  ]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
});

test("the account Worker projects opaque sandbox preview capabilities", () => {
  assert.equal(isManagedRoutePath("/sandbox-preview/capability/"), true);
  assert.equal(isManagedRoutePath("/sandbox-preview/capability/assets/app.js"), true);
  assert.equal(isManagedRoutePath("/sandbox-preview/"), false);
});

test("the removed model capabilities route is not projected", () => {
  assert.equal(isManagedRoutePath("/v1/model-capabilities"), false);
});

test("the account hand WebSocket stays on the managed service boundary", async () => {
  assert.equal(isManagedRoutePath("/v1/account/tool-host"), true);
  const request = new Request("https://nanocodex.localhost/v1/account/tool-host", {
    headers: { upgrade: "websocket" },
  });
  let forwarded: Request | undefined;
  const response = await routeManaged(request, {
    NANOCODEX_BACKEND: {
      fetch(candidate: Request) {
        forwarded = candidate;
        return Promise.resolve(new Response(null, { status: 204 }));
      },
      connect() { throw new Error("unused"); },
    },
  }, new URL(request.url));

  assert.equal(response?.status, 204);
  assert.equal(forwarded, request);
});

test("interactive screen routes retain their exact managed boundary", () => {
  for (const suffix of ["", "/screens", "/host", "/view", "/renew", "/ice"]) {
    assert.equal(isManagedRoutePath("/v1/account/hands" + suffix), true);
  }
  for (const suffix of ["/", "/host/extra", "/input", "/command", "-other"]) {
    assert.equal(isManagedRoutePath("/v1/account/hands" + suffix), false);
  }
});

test("VM host WebSockets stay on their exact managed service boundaries", () => {
  for (const path of [
    "/v1/account/vm-host",
    "/v1/agents/agent-1/vm-host",
    "/v1/system/vm-host",
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/tool-host`,
    ...["host", "ice", "renew"].map(endpoint => `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/hands/${endpoint}`),
  ]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }

  for (const path of [
    "/v1/account/vm-host/",
    "/v1/system/vm-host/",
    "/v1/system/vm-host/extra",
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/tool-host/extra`,
    `/v1/vm-host-attachments/${"p".repeat(43)}/11111111-1111-4111-8111-111111111111/hands/view`,
  ]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
});


test("account Hand discovery uses only the exact managed route", async () => {
  assert.equal(isManagedRoutePath("/v1/account/hands"), true);
  assert.equal(isManagedRoutePath("/v1/account/hands/"), false);
  assert.equal(isManagedRoutePath("/v1/account/hands/other"), false);
  const request = new Request("https://nanocodex.localhost/v1/account/hands", { headers: { authorization: "Bearer test" } });
  let forwarded: Request | undefined;
  const response = await routeManaged(request, { NANOCODEX_BACKEND: {
    fetch(candidate: Request) { forwarded = candidate; return Promise.resolve(Response.json({ data: [] })); },
    connect() { throw new Error("unused"); },
  } }, new URL(request.url));
  assert.equal(forwarded, request);
  assert.deepEqual(await response?.json(), { data: [] });
});


test("server Hand enrollment and scoped publishers retain their managed boundary", async () => {
  const owner = "11111111-1111-4111-8111-111111111111";
  const id = "22222222-2222-4222-8222-222222222222";
  const management = `/v1/account/hand-hosts/${id}`;
  const publisher = `/v1/hand-hosts/${owner}/${id}/hands`;
  for (const path of ["/v1/account/hand-hosts", management, ...["host", "ice", "renew"].map(endpoint => `${publisher}/${endpoint}`)]) {
    assert.equal(isManagedRoutePath(path), true, path);
  }
  for (const path of ["/v1/account/hand-hosts/", `${management}/extra`, "/v1/account/hand-hosts/invalid", `${publisher}/view`, `${publisher}/host/extra`, `${publisher}/`, `/v1/hand-hosts/${owner}/invalid/hands/host`]) {
    assert.equal(isManagedRoutePath(path), false, path);
  }
  for (const request of [
    new Request(`https://nanocodex.localhost${management}`, { method: "PUT", headers: { authorization: "Bearer account-test", origin: "https://nanocodex.localhost", "content-type": "application/json" }, body: JSON.stringify({ name: "SSH fixture" }) }),
    new Request(`https://nanocodex.localhost${publisher}/host`, { headers: { authorization: "Bearer publisher-test", upgrade: "websocket" } }),
    new Request(`https://nanocodex.localhost${publisher}/ice`, { method: "POST", headers: { authorization: "Bearer publisher-test" } }),
  ]) {
    let forwarded: Request | undefined;
    const response = await routeManaged(request, { NANOCODEX_BACKEND: {
      fetch(candidate: Request) { forwarded = candidate; return Promise.resolve(new Response(null, { status: 204 })); },
      connect() { throw new Error("unused"); },
    } }, new URL(request.url));
    assert.equal(response?.status, 204);
    assert.equal(forwarded, request);
  }
});


test("screen proxy timing preserves authentication headers, response identity and private query data", async () => {
  const request = new Request("https://nanocodex.localhost/v1/account/hands/view?generation=private-generation", {
    headers: { upgrade: "websocket", authorization: "Bearer private-key", "x-nanocodex-access": "private-snapshot" },
  });
  const response = new Response(null, { status: 204, headers: { "x-nanocodex-request-id": "correlation-id",
    "x-nanocodex-access-rejected": "1", "server-timing": 'managed_auth;dur=0.2;desc="access"' } });
  let forwarded: Request | undefined;
  const logs: unknown[] = [];
  const original = console.info;
  console.info = message => { logs.push(message); };
  try {
    const result = await routeManaged(request, { NANOCODEX_BACKEND: {
      async fetch(candidate: Request) { forwarded = candidate; return response; },
      connect() { throw new Error("unused"); },
    } }, new URL(request.url));
    assert.equal(forwarded, request);
    assert.equal(result, response, "An upgraded response must retain its exact socket and headers");
    assert.equal(result.headers.get("x-nanocodex-access-rejected"), "1");
    assert.equal(logs.length, 1);
    assert.equal((logs[0] as { request_id: string }).request_id, "correlation-id");
    assert.equal(typeof (logs[0] as { backend_ms: number }).backend_ms, "number");
    const span = logs[0] as { started_at_ms: number; finished_at_ms: number };
    assert.ok(span.started_at_ms > 0);
    assert.ok(span.finished_at_ms >= span.started_at_ms);
    assert.equal(JSON.stringify(logs).includes("private-"), false);
  } finally { console.info = original; }
});

const localPrincipal = { kind: "api_key" as const, userId: "owner", organizationId: "org", teamId: "team",
  authorizationEpoch: 1, capabilities: ["agents:read", "tools:use"] };
const localSecret = "local-viewer-fixture-secret-at-least-thirty-two-bytes";
async function cachedViewer(identity = localPrincipal, extra: Record<string, string> = {}, age = 0) {
  const { createManagedAccessClaims, signManagedAccessClaims } = await import("nanocodex/cloudflare/managed-access");
  const source = new Request("https://account.test/v1/account/hands/screens", { headers: { authorization: "Bearer fixture", ...extra } });
  const claims = await createManagedAccessClaims(source, identity, Date.now() - age);
  const token = await signManagedAccessClaims(claims, { NANOCODEX_ACCESS_SECRET: localSecret });
  return new Request("https://account.test/v1/account/hands/view?generation=fixture", {
    headers: { ...Object.fromEntries(source.headers), upgrade: "websocket", "x-nanocodex-access": token },
  });
}
function localEnvironment(broker: (request: Request) => Promise<Response>, backend: (request: Request) => Promise<Response>) {
  return { NANOCODEX_ACCESS_SECRET: localSecret,
    NANOCODEX_HAND_BROKER: { getByName(owner: string) { assert.equal(owner, localPrincipal.userId); return { fetch: broker }; } } as unknown as DurableObjectNamespace,
    NANOCODEX_BACKEND: { fetch: backend, connect() { throw new Error("unused"); } } as unknown as Fetcher };
}

test("verified viewer authority skips the managed service and addresses only its authenticated account", async () => {
  const request = await cachedViewer();
  let brokerCalls = 0;
  const response = await routeManaged(request, localEnvironment(async forwarded => {
    brokerCalls++;
    assert.equal(forwarded.url, "https://account-tools.internal/hands/view?generation=fixture");
    assert.equal(forwarded.headers.get("x-nanocodex-owner-id"), "owner");
    return new Response(null, { status: 204 });
  }, async () => { throw new Error("managed service must not be called"); }), new URL(request.url));
  assert.equal(brokerCalls, 1);
  assert.equal(response?.status, 204);
  assert.match(response!.headers.get("server-timing")!, /desc="access"/);
  assert.equal(response!.headers.has("x-nanocodex-access"), false, "reuse cannot extend authority");
});

test("unusable viewer snapshots and disallowed scope preserve the original managed rejection path", async () => {
  const valid = await cachedViewer();
  const browser = { ...localPrincipal, kind: "account_session" } as unknown as typeof localPrincipal;
  const requests = [
    new Request(valid, { headers: { authorization: "Bearer fixture", upgrade: "websocket" } }),
    new Request(valid, { headers: { ...Object.fromEntries(valid.headers), "x-nanocodex-access": "invalid" } }),
    new Request(valid, { headers: { ...Object.fromEntries(valid.headers), authorization: "Bearer changed" } }),
    new Request(valid, { headers: { ...Object.fromEntries(valid.headers), cookie: "nanocodex_account=added" } }),
    await cachedViewer(localPrincipal, {}, 120_001),
    await cachedViewer(localPrincipal, {}, -30_000),
    await cachedViewer({ ...localPrincipal, capabilities: ["agents:read"] }),
    await cachedViewer({ ...localPrincipal, connectGrant: { grantId: "grant" } } as typeof localPrincipal),
    await cachedViewer(browser, { cookie: "nanocodex_account=fixture" }),
    await cachedViewer(browser, { cookie: "nanocodex_account=fixture", origin: "https://evil.test" }),
    new Request("https://other.test/v1/account/hands/view", valid),
    new Request("https://account.test/v1/account/hands/renew", valid),
    new Request("https://account.test/v1/account/hands/host", valid),
  ];
  for (const request of requests) {
    let calls = 0;
    const response = await routeManaged(request, localEnvironment(async () => { throw new Error("broker must not be reached"); }, async forwarded => {
      calls++; assert.equal(forwarded, request);
      return new Response(null, { status: 401, headers: { "x-nanocodex-access-rejected": "1" } });
    }), new URL(request.url));
    assert.equal(calls, 1);
    assert.equal(response?.headers.get("x-nanocodex-access-rejected"), "1");
  }
  for (const secret of [undefined, "short", "rotated-secret-at-least-thirty-two-characters"]) {
    let calls = 0;
    const env = localEnvironment(async () => { throw new Error("broker must not be reached"); }, async forwarded => {
      calls++; assert.equal(forwarded, valid); return new Response(null, { status: 401 });
    });
    await routeManaged(valid, { ...env, NANOCODEX_ACCESS_SECRET: secret }, new URL(valid.url));
    assert.equal(calls, 1);
  }
});

test("direct broker failure and stale generation never replay through the managed service", async () => {
  for (const throws of [false, true]) {
    const request = await cachedViewer(); let brokerCalls = 0; let backendCalls = 0;
    const response = await routeManaged(request, localEnvironment(async () => {
      brokerCalls++; if (throws) throw new Error("broker disconnected");
      return new Response(null, { status: 409 });
    }, async () => { backendCalls++; return new Response(null, { status: 204 }); }), new URL(request.url));
    assert.equal(brokerCalls, 1); assert.equal(backendCalls, 0);
    assert.equal(response?.status, throws ? 503 : 409);
    assert.equal(response?.headers.has("x-nanocodex-access-rejected"), false);
  }
});

test("cloud phone controls and signed callbacks reach managed authentication", () => {
  const id = "11111111-1111-4111-8111-111111111111";
  for (const path of ["health", "check", "calls", `calls/${id}`, `calls/${id}/hangup`, `calls/${id}/steer`, `status/${id}`, `media/${id}/`, "internal/state", "internal/setup"])
    assert.equal(isManagedRoutePath(`/v1/phone/bridge/${path}`), true);
  for (const path of ["", "internal/secrets", "calls/invalid", `media/${id}`])
    assert.equal(isManagedRoutePath(`/v1/phone/bridge/${path}`), false);
});

test("standalone inference routes project through the managed service", async () => {
  for (const path of ["/v1/models", "/v1/responses", "/v1/inference/models", "/v1/inference/sessions", "/v1/inference/responses", "/v1/inference/keys"]) {
    assert.equal(isManagedRoutePath(path), true);
    const request = new Request("https://nanocodex.example" + path, { headers: { authorization: "Bearer nci_live_synthetic" } });
    let forwarded: Request | undefined;
    const response = await routeManaged(request, { NANOCODEX_BACKEND: {
      fetch(candidate: Request) { forwarded = candidate; return Promise.resolve(new Response(null, { status: 204 })); },
      connect() { throw new Error("unused"); },
    } }, new URL(request.url));
    assert.equal(response?.status, 204);
    assert.equal(forwarded, request);
  }
});

test("standard inference aliases project only their exact paths", async () => {
  for (const path of ["/v1/responses/", "/v1/responses/response-id", "/v1/responses-other",
    "/v1/models/", "/v1/models/model-id", "/v1/models-other", "/v1/chat/completions", "/v1/sessions"]) {
    assert.equal(isManagedRoutePath(path), false, path);
    const request = new Request("https://nanocodex.example" + path, {
      headers: { authorization: "Bearer nci_live_synthetic", cookie: "synthetic=owner" },
    });
    const response = await routeManaged(request, { NANOCODEX_BACKEND: {
      fetch() { throw new Error("unrecognized alias must not forward"); },
      connect() { throw new Error("unused"); },
    } }, new URL(request.url));
    assert.equal(response, undefined, path);
  }
});

test("inference credentials cannot reach account, connector, agent or hand proxy paths", async () => {
  for (const path of ["/v1/me", "/v1/agents", "/v1/api-keys", "/v1/connectors/github", "/v1/credentials",
    "/v1/account/hands", "/v1/account/hands/screens", "/v1/account/tool-host", "/v1/history", "/v1/memory", "/v1/egress", "/v1/wallet"]) {
    const request = new Request("https://nanocodex.example" + path, {
      headers: { authorization: "Bearer nci_live_synthetic", cookie: "synthetic=account", upgrade: "websocket", "x-nanocodex-managed-access": "synthetic" },
    });
    const response = await routeManaged(request, { NANOCODEX_BACKEND: {
      fetch() { throw new Error("inference credential escaped the proxy boundary"); },
      connect() { throw new Error("unused"); },
    } }, new URL(request.url));
    assert.equal(response?.status, 403, path);
    assert.deepEqual(await response.json(), { error: "inference_key_scope" });
  }
});

test("malformed inference authorization cannot fall back to a cached owner cookie", async () => {
  for (const authorization of ["Basic nci_live_synthetic", "nci_live_synthetic", "bearer NCI_LIVE_synthetic"]) {
    const request=new Request("https://nanocodex.example/v1/account/hands", {headers:{authorization,cookie:"synthetic=owner"}});
    const response=await routeManaged(request, {NANOCODEX_BACKEND:{fetch(){throw Error("must not forward");},connect(){throw Error("unused");}}}, new URL(request.url));
    assert.equal(response?.status,403);
  }
});

test("agent lifecycle timings correlate across Workers without touching responses or logging private inputs", async () => {
  const id = "11111111-1111-4111-8111-111111111111";
  for (const path of ["/v1/agents", "/v1/agents/live", ...["", "/routing", "/settings", "/prepare", "/ws", "/events", "/events/history", "/turns", "/turns/fixture-turn/cancel"].map(suffix => `/v1/agents/${id}${suffix}`)]) {
    const request = new Request(`https://nanocodex.example${path}?cursor=private-cursor`, {
      method: "POST", headers: { authorization: "Bearer private-key", "x-nanocodex-access": "private-access" }, body: "private-body",
    });
    const response = new Response("private-response", { headers: { "x-nanocodex-request-id": "timing-fixture" } });
    const logs: Record<string, unknown>[] = [];
    const original = console.info;
    console.info = value => { logs.push(value); };
    try {
      const result = await routeManaged(request, { NANOCODEX_BACKEND: {
        async fetch(forwarded: Request) { assert.equal(forwarded, request); return response; },
        connect() { throw new Error("unused"); },
      } }, new URL(request.url));
      assert.equal(result, response);
      assert.equal(result.bodyUsed, false);
      assert.equal(logs.length, 1);
      assert.equal(logs[0].type, "managed.proxy");
      assert.equal(logs[0].request_id, "timing-fixture");
      assert.equal(logs[0].path, path);
      assert.equal(typeof logs[0].backend_ms, "number");
      assert.ok(Number(logs[0].finished_at_ms) >= Number(logs[0].started_at_ms));
      assert.equal(JSON.stringify(logs).includes("private-"), false);
    } finally { console.info = original; }
  }
});


test("inference lifecycle observation failures preserve the backend response", async () => {
  const id = "11111111-1111-4111-8111-111111111111";
  for (const suffix of ["turns", "turns/fixture-turn/cancel", "events", "ws"]) {
    const request = new Request(`https://nanocodex.example/v1/agents/${id}/${suffix}`);
    const response = new Response("untouched stream", { status: 202 });
    const original = console.info;
    console.info = () => { throw new Error("synthetic logger failure"); };
    try {
      const result = await routeManaged(request, { NANOCODEX_BACKEND: {
        async fetch() { return response; }, connect() { throw new Error("unused"); },
      } }, new URL(request.url));
      assert.equal(result, response);
      assert.equal(result.bodyUsed, false);
    } finally { console.info = original; }
  }
});

const browserPrincipal = { ...localPrincipal, kind: "account_session" as const };
const handCookieName = "__Secure-nanocodex_hand_access";
const browserHeaders = { cookie: "nanocodex_account=browser-session", origin: "https://account.test", "sec-fetch-site": "same-origin" };
async function browserSnapshot(source = new Request("https://account.test/v1/account/hands/screens", { headers: browserHeaders }),
  identity: import("nanocodex/cloudflare/managed-access").ManagedAccessPrincipal = browserPrincipal, age = 0) {
  const { createManagedAccessClaims, signManagedAccessClaims } = await import("nanocodex/cloudflare/managed-access");
  const claims = await createManagedAccessClaims(source, identity, Date.now() - age);
  return { source, token: await signManagedAccessClaims(claims, { NANOCODEX_ACCESS_SECRET: localSecret }) };
}
function browserViewer(token: string, extra: Record<string, string> = {}, url = "https://account.test/v1/account/hands/view?generation=private-generation") {
  return new Request(url, { headers: { ...browserHeaders, upgrade: "websocket",
    cookie: `${browserHeaders.cookie}; ${handCookieName}=${token}`, ...extra } });
}
function snapshotResponse(token: string, ttl = "119999") {
  return new Response("screens", { headers: { "x-nanocodex-access": token, "x-nanocodex-access-ttl-ms": ttl,
    "set-cookie": "unrelated=preserved; Secure; HttpOnly", "x-nanocodex-request-id": "browser-fixture" } });
}

test("browser discovery and ICE carry verified account snapshots in a bounded host-only HttpOnly cookie", async () => {
  for (const endpoint of ["screens", "ice"]) {
    const source = new Request(`https://account.test/v1/account/hands/${endpoint}`, { method: endpoint === "ice" ? "POST" : "GET",
      headers: browserHeaders, ...(endpoint === "ice" ? { body: "private-request-body" } : {}) });
    const { token } = await browserSnapshot(source);
    const backendResponse = snapshotResponse(token, "87654");
    const response = await routeManaged(source, localEnvironment(async () => { throw Error("not a viewer"); }, async request => {
      assert.equal(request, source);
      if (endpoint === "ice") assert.equal(await request.text(), "private-request-body");
      return backendResponse;
    }), new URL(source.url));
    assert.equal(response?.status, 200);
    assert.equal(response?.bodyUsed, false);
    assert.equal(await response?.text(), "screens");
    const cookies = response!.headers.getSetCookie();
    assert.equal(cookies[0], "unrelated=preserved; Secure; HttpOnly");
    assert.equal(cookies[1], `${handCookieName}=${token}; Max-Age=87; Path=/v1/account/hands; Secure; HttpOnly; SameSite=Strict`);
    assert.equal(response!.headers.get("cache-control"), "no-store");
    assert.equal(source.headers.has("x-nanocodex-access"), false);
    assert.equal(response!.headers.get("x-nanocodex-request-id"), "browser-fixture");
  }
});

test("cookie viewer admission skips the managed hop without changing original auth inputs or leaking authority", async () => {
  const { token } = await browserSnapshot();
  const request = browserViewer(token);
  const logs: unknown[] = []; const original = console.info;
  console.info = value => { logs.push(value); };
  try {
    const response = await routeManaged(request, localEnvironment(async forwarded => {
      assert.equal(forwarded.headers.get("cookie"), request.headers.get("cookie"));
      assert.equal(forwarded.headers.has("authorization"), false);
      assert.equal(forwarded.headers.has("x-nanocodex-access"), false);
      assert.equal(forwarded.headers.get("origin"), "https://account.test");
      assert.equal(forwarded.headers.get("x-nanocodex-authorization-epoch"), "1");
      return new Response(null, { status: 204 });
    }, async () => { throw Error("unexpected managed hop"); }), new URL(request.url));
    assert.equal(response?.status, 204);
    assert.equal(response!.headers.has("set-cookie"), false, "reuse cannot extend the snapshot lifetime");
    assert.equal(request.headers.has("x-nanocodex-access"), false);
    assert.equal(request.headers.has("authorization"), false);
    assert.equal((logs[0] as { route: string }).route, "local_access");
    for (const privateValue of [token, "browser-session", "private-generation"]) assert.equal(JSON.stringify(logs).includes(privateValue), false);
  } finally { console.info = original; }
});

test("unusable browser snapshots retain the exact original live-auth request", async () => {
  const { token } = await browserSnapshot();
  const expired = await browserSnapshot(undefined, browserPrincipal, 120_001);
  const future = await browserSnapshot(undefined, browserPrincipal, -30_000);
  const denied = await browserSnapshot(undefined, { ...browserPrincipal, capabilities: ["agents:read"] });
  const connect = await browserSnapshot(undefined, { ...browserPrincipal, connectGrant: { grantId: "restricted" } });
  const api = await browserSnapshot(new Request("https://account.test/v1/account/hands/screens", {
    headers: { ...browserHeaders, authorization: "Bearer fixture" } }), localPrincipal);
  const cases = [
    browserViewer(token + "x"), browserViewer("invalid"), browserViewer(expired.token), browserViewer(future.token),
    browserViewer(denied.token), browserViewer(connect.token),
    browserViewer(api.token, { authorization: "Bearer fixture" }),
    browserViewer(token, { cookie: `${handCookieName}=${token}` }),
    browserViewer(token, { cookie: `nanocodex_account=changed; ${handCookieName}=${token}` }),
    browserViewer(token, { cookie: `nanocodex_account=changed; ${browserHeaders.cookie}; ${handCookieName}=${token}` }),
    browserViewer(token, { cookie: `${browserHeaders.cookie}; ${handCookieName}=${token}; ${handCookieName}=${token}` }),
    browserViewer(token, { cookie: `${browserHeaders.cookie}; ${handCookieName}=invalid; ${handCookieName}=${token}` }),
    browserViewer(token, { origin: "https://evil.test" }), browserViewer(token, { origin: "" }),
    browserViewer(token, { "sec-fetch-site": "cross-site" }), browserViewer(token, { "sec-fetch-site": "same-site" }),
    browserViewer(token, { authorization: "Bearer added" }), browserViewer(token, { "x-nanocodex-connect-user": "added" }),
    browserViewer(token, { origin: "https://other.test" }, "https://other.test/v1/account/hands/view"),
    browserViewer(token, { origin: "http://account.test" }, "http://account.test/v1/account/hands/view"),
    browserViewer(token, { "x-nanocodex-access": "invalid-explicit-header" }),
    ...["host", "renew", "screens", "ice"].map(route => browserViewer(token, {}, `https://account.test/v1/account/hands/${route}`)),
  ];
  for (const request of cases) {
    let calls = 0;
    const response = await routeManaged(request, localEnvironment(async () => { throw Error("must not admit"); }, async forwarded => {
      calls++; assert.equal(forwarded, request);
      return new Response(null, { status: 401, headers: { "x-nanocodex-access-rejected": "1" } });
    }), new URL(request.url));
    assert.equal(calls, 1, request.url);
    assert.equal(response?.status, 401);
    assert.equal(response!.headers.get("x-nanocodex-access-rejected"), "1");
  }
  for (const secret of [undefined, "short", "rotated-secret-at-least-thirty-two-characters"]) {
    const request = browserViewer(token); let calls = 0;
    const env = localEnvironment(async () => { throw Error("must not admit"); }, async forwarded => {
      calls++; assert.equal(forwarded, request); return new Response(null, { status: 401 });
    });
    await routeManaged(request, { ...env, NANOCODEX_ACCESS_SECRET: secret }, new URL(request.url));
    assert.equal(calls, 1);
  }
});

test("only successful verified browser Hand responses can seed the admission cookie", async () => {
  const { source, token } = await browserSnapshot();
  const apiSource = new Request(source, { headers: { authorization: "Bearer fixture" } });
  const api = await browserSnapshot(apiSource, localPrincipal);
  const connectSource = new Request("https://nanocodex.internal/v1/account/hands/screens", { headers: { "x-nanocodex-connect-user": "owner" } });
  const connect = await browserSnapshot(connectSource, { ...localPrincipal, kind: "connect_grant", connectGrant: { grantId: "grant" } });
  const denied = await browserSnapshot(source, { ...browserPrincipal, capabilities: ["agents:read"] });
  const restricted = await browserSnapshot(source, { ...browserPrincipal, connectGrant: { grantId: "grant" } });
  const expired = await browserSnapshot(source, browserPrincipal, 120_001);
  const cases: [Request, Response][] = [
    [source, snapshotResponse("invalid")], [source, snapshotResponse(token + "x")], [source, snapshotResponse(expired.token)],
    [source, snapshotResponse(denied.token)], [source, snapshotResponse(restricted.token)],
    [apiSource, snapshotResponse(api.token)], [connectSource, snapshotResponse(connect.token)],
    [new Request(source, { headers: {} }), snapshotResponse(token)],
    [new Request(source, { headers: { ...browserHeaders, cookie: "nanocodex_account=changed" } }), snapshotResponse(token)],
    [new Request(source, { headers: { ...browserHeaders, origin: "https://evil.test" } }), snapshotResponse(token)],
    [new Request(source, { headers: { ...browserHeaders, "sec-fetch-site": "cross-site" } }), snapshotResponse(token)],
    [new Request(source, { method: "POST" }), snapshotResponse(token)],
    [new Request("http://account.test/v1/account/hands/screens", source), snapshotResponse(token)],
    [new Request("https://other.test/v1/account/hands/screens", source), snapshotResponse(token)],
    ...["host", "view", "renew", ""].map(route => [new Request(`https://account.test/v1/account/hands${route ? "/" + route : ""}`, source), snapshotResponse(token)] as [Request, Response]),
    ...["", "0", "-1", "999", "Infinity", "NaN"].map(ttl => [source, snapshotResponse(token, ttl)] as [Request, Response]),
    ...[401, 403, 500].map(status => [source, new Response(null, { status, headers: { "x-nanocodex-access": token, "x-nanocodex-access-ttl-ms": "120000" } })] as [Request, Response]),
  ];
  for (const [request, backendResponse] of cases) {
    const response = await routeManaged(request, localEnvironment(async () => { throw Error("not a viewer"); }, async forwarded => {
      assert.equal(forwarded, request); return backendResponse;
    }), new URL(request.url));
    assert.equal(response, backendResponse, request.url);
    assert.equal(response!.headers.getSetCookie().some(cookie => cookie.startsWith(handCookieName + "=")), false);
  }
});

test("cookie TTL never exceeds returned remaining lifetime or the existing 120 second bound", async () => {
  const { source, token } = await browserSnapshot();
  for (const [ttl, expected] of [["999999", 120], ["120000", 120], ["1000", 1], ["1999", 1]] as const) {
    const response = await routeManaged(source, localEnvironment(async () => { throw Error("not a viewer"); }, async () => snapshotResponse(token, ttl)), new URL(source.url));
    assert.match(response!.headers.getSetCookie()[1], new RegExp(`; Max-Age=${expected};`));
  }
});

test("live renewal and broker authorization failures clear browser access without replaying or extending it", async () => {
  const { token } = await browserSnapshot();
  for (const status of [401, 403]) {
    for (const endpoint of ["view", "renew"]) {
      const request = endpoint === "view" ? browserViewer(token) : new Request("https://account.test/v1/account/hands/renew", {
        method: "POST", headers: { ...browserHeaders, cookie: `${browserHeaders.cookie}; ${handCookieName}=${token}` }, body: '{"connection_id":"revoked-fixture"}',
      });
      let brokerCalls = 0; let backendCalls = 0;
      const rejected = () => new Response("revoked", { status, headers: { "x-nanocodex-request-id": "rejection-fixture" } });
      const response = await routeManaged(request, localEnvironment(async () => { brokerCalls++; return rejected(); }, async forwarded => {
        backendCalls++; assert.equal(forwarded, request); assert.equal(forwarded.headers.has("x-nanocodex-access"), false); return rejected();
      }), new URL(request.url));
      assert.equal(brokerCalls, endpoint === "view" ? 1 : 0);
      assert.equal(backendCalls, endpoint === "renew" ? 1 : 0);
      assert.equal(response?.status, status);
      assert.equal(await response?.text(), "revoked");
      assert.equal(response!.headers.get("set-cookie"), `${handCookieName}=; Max-Age=0; Path=/v1/account/hands; Secure; HttpOnly; SameSite=Strict`);
      if (endpoint === "renew") assert.equal(response!.headers.get("x-nanocodex-request-id"), "rejection-fixture");
      else assert.ok(response!.headers.get("x-nanocodex-request-id"));
    }
  }
});
