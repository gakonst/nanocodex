import assert from "node:assert/strict";
import test from "node:test";

import { Client, Dialog, Principal, Transport } from "../cloud/index.mjs";
import { HostPrincipal } from "../cloud/server/index.mjs";

const exchangeToken = "e".repeat(43);
const principalId = "p".repeat(43);
const appId = "host-app";
const appOrigin = "https://host.example";
const secret = "project-secret-that-is-long-enough-123";
const issuer = "better-auth";
const tenant = "acme";

test("host principal reads only a bounded exchange from an exact same-origin route", async () => {
  const requests = [];
  const expiresAt = Math.floor(Date.now() / 1_000) + 60;
  const principal = Principal.host({
    async fetch(input, init) {
      requests.push(new Request(input, init));
      return Response.json({ token: exchangeToken, expires_at: expiresAt });
    },
  }).setup({ appId, appOrigin });

  assert.deepEqual(await principal.create({ resources: ["urn:example:read"] }), {
    token: exchangeToken,
    expiresAt,
  });
  assert.equal(requests[0].url, `${appOrigin}/api/nanocodex/host-principal`);
  assert.equal(requests[0].method, "POST");
  assert.equal(requests[0].credentials, "include");
  assert.equal(requests[0].headers.get("authorization"), null);
  assert.equal(requests[0].headers.get("x-nanocodex-app-id"), appId);
  assert.deepEqual(await requests[0].json(), { resources: ["urn:example:read"] });

  assert.throws(() => Principal.host({
    url: "https://attacker.example/exchange",
    fetch() { throw new Error("must not fetch"); },
  }).setup({ appId, appOrigin }), /same|origin/);
});

test("host principal rejects expired and overlong exchanges", async () => {
  const now = Math.floor(Date.now() / 1_000);
  for (const expires_at of [now, now + 301]) {
    const principal = Principal.host({
      fetch: async () => Response.json({ token: exchangeToken, expires_at }),
    }).setup({ appId, appOrigin });
    await assert.rejects(
      principal.create({ resources: ["urn:example:read"] }),
      /invalid exchange/,
    );
  }
});

test("server helper keeps the project secret server-side and binds issuer, tenant, subject, session, origin, and resources", async () => {
  const requests = [];
  const expiresAt = Math.floor(Date.now() / 1_000) + 60;
  const host = HostPrincipal.create({
    appId,
    appOrigin,
    secret,
    baseUrl: "https://connect.example",
    async fetch(input, init) {
      const request = new Request(input, init);
      requests.push(request);
      if (request.method === "DELETE") return new Response(null, { status: 204 });
      return Response.json({ token: exchangeToken, expires_at: expiresAt }, { status: 201 });
    },
  });

  assert.deepEqual(await host.create({
    issuer,
    tenant,
    subject: "better-auth-user-7",
    sessionId: "better-auth-session-9",
    resources: ["urn:example:read"],
    expiresIn: 60,
  }), { token: exchangeToken, expiresAt });
  await assert.rejects(host.create({
    issuer,
    tenant,
    subject: "better-auth-user-7",
    sessionId: "better-auth-session-9",
    resources: ["urn:nanocodex:access-key:create"],
  }), /non-exchange strings/);
  assert.equal(requests[0].url, "https://connect.example/v1/host-principal/exchanges");
  assert.equal(requests[0].headers.get("authorization"), `Bearer ${secret}`);
  assert.equal(requests[0].headers.get("x-nanocodex-app-id"), appId);
  assert.equal(requests[0].headers.has("origin"), false);
  assert.deepEqual(await requests[0].json(), {
    app_origin: appOrigin,
    issuer,
    tenant,
    subject: "better-auth-user-7",
    session_id: "better-auth-session-9",
    resources: ["urn:example:read"],
    expires_in: 60,
  });

  await host.revoke({
    issuer,
    tenant,
    subject: "better-auth-user-7",
    sessionId: "better-auth-session-9",
  });
  assert.equal(requests[1].url, "https://connect.example/v1/host-principal/sessions");
  assert.equal(requests[1].method, "DELETE");
  assert.deepEqual(await requests[1].json(), {
    app_origin: appOrigin,
    issuer,
    tenant,
    subject: "better-auth-user-7",
    session_id: "better-auth-session-9",
  });
});

test("server helper requires every host identity claim within its explicit 512-character bound", async () => {
  const host = HostPrincipal.create({
    appId,
    appOrigin,
    secret,
    async fetch() {
      throw new Error("invalid claims must not reach the service");
    },
  });
  const claims = { issuer, tenant, subject: "user-7", sessionId: "session-9" };
  for (const name of Object.keys(claims)) {
    for (const value of [
      "",
      "x".repeat(513),
      "control\u0000claim",
      undefined,
      null,
      true,
      7,
      [],
      {},
    ]) {
      await assert.rejects(
        host.create({ ...claims, [name]: value, resources: ["urn:example:read"] }),
        /bounded opaque issuer, tenant, subject, and sessionId/,
      );
      await assert.rejects(
        host.revoke({ ...claims, [name]: value }),
        /bounded opaque issuer, tenant, subject, and sessionId/,
      );
    }
  }
});

test("server handler authenticates an exact-origin POST and returns no claims", async () => {
  let authenticated = 0;
  const expiresAt = Math.floor(Date.now() / 1_000) + 60;
  const host = HostPrincipal.create({
    appId,
    appOrigin,
    secret,
    async fetch() {
      return Response.json({
        token: exchangeToken,
        expires_at: expiresAt,
      }, { status: 201 });
    },
  });
  const handler = host.handler({
    authenticate(request) {
      authenticated += 1;
      assert.equal(request.headers.get("cookie"), "host-session=valid");
      return { issuer, tenant, subject: "user-7", sessionId: "session-9" };
    },
  });

  const rejected = await handler(new Request(`${appOrigin}/api/nanocodex/host-principal`, {
    method: "POST",
    headers: { origin: "https://attacker.example", cookie: "host-session=valid" },
    body: JSON.stringify({ resources: ["urn:example:read"] }),
  }));
  assert.equal(rejected.status, 403);
  assert.equal(authenticated, 0);

  const accepted = await handler(new Request(`${appOrigin}/api/nanocodex/host-principal`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      origin: appOrigin,
      "sec-fetch-site": "same-origin",
      cookie: "host-session=valid",
    },
    body: JSON.stringify({ resources: ["urn:example:read"] }),
  }));
  assert.equal(accepted.status, 200);
  assert.equal(accepted.headers.get("cache-control"), "no-store");
  assert.deepEqual(await accepted.json(), {
    token: exchangeToken,
    expires_at: expiresAt,
  });
  assert.equal(authenticated, 1);
});

test("Connect host principal is hosted-only, carries exact exchange context, and has no wallet address", async () => {
  const principalRequests = [];
  const providerRequests = [];
  const controlRequests = [];
  const expiresAt = Math.floor(Date.now() / 1_000) + 60;
  const principal = Principal.host({
    async fetch(input, init) {
      principalRequests.push(new Request(input, init));
      return Response.json({ token: exchangeToken, expires_at: expiresAt });
    },
  });
  const provider = {
    async request(request) {
      providerRequests.push(request);
      return {
        accounts: [{
          principal: { kind: "host", id: principalId },
          capabilities: { auth: { approval_id: "approval-host" } },
        }],
      };
    },
  };
  const transport = Transport.from({
    key: "host-test",
    name: "host-test",
    type: "host-test",
    setup() {
      return {
        baseUrl: "https://connect.example",
        async request(request) {
          controlRequests.push(request);
          return hostConnectionWire();
        },
      };
    },
  });
  const client = Client.create({
    appId,
    appOrigin,
    auth: { resources: [`urn:nanocodex:host-principal:exchange:${"x".repeat(43)}`] },
    dialog: Dialog.memory(),
    principal,
    provider,
    session: false,
    transport,
  });

  const connection = await client.connection.connect({
    capabilities: { cloudAccounts: { github: true } },
  });
  const baseResources = (await principalRequests[0].json()).resources;
  assert.equal(baseResources.some((resource) => resource.includes("x".repeat(43))), false);
  assert.ok(baseResources.includes("urn:nanocodex:authorization:hosted"));
  const signedResources = providerRequests[0].params[0].capabilities.auth.resources;
  assert.deepEqual(signedResources, [
    ...baseResources,
    `urn:nanocodex:host-principal:exchange:${exchangeToken}`,
  ]);
  assert.deepEqual(providerRequests[0].context.hostPrincipal, { token: exchangeToken, expiresAt });
  assert.equal("authorizeAccessKey" in providerRequests[0].params[0].capabilities, false);
  assert.deepEqual(controlRequests[0].body, {
    app_id: appId,
    principal: { kind: "host", id: principalId },
    approval_id: "approval-host",
    authorization_mode: "hosted",
    permission: "agent.run",
    requested_connectors: ["github"],
  });
  assert.deepEqual(connection.principal, { kind: "host", id: principalId });
  assert.equal(connection.accountAddress, undefined);
  assert.equal(connection.authorization, "hosted");
  assert.equal(connection.accessKey, undefined);
  assert.equal(connection.mpp, undefined);

  await assert.rejects(client.connection.connect({ authorization: "access_key" }), /host principal.*hosted/);
  await assert.rejects(client.connection.connect({
    capabilities: { authorizeAccessKey: { expiry: expiresAt } },
  }), /cannot request access-key or MPP/);
  await assert.rejects(Client.create({
    appId,
    appOrigin,
    auth: { resources: ["urn:nanocodex:mpp:machusd:spend"] },
    dialog: Dialog.memory(),
    principal,
    provider,
    session: false,
    transport,
  }).connection.connect({}), /cannot request access-key or MPP/);
  assert.equal(principalRequests.length, 1);
});

test("host reconnect consumes a fresh exact-resource exchange and fences account switches", async () => {
  const storage = memoryStorage();
  const tokens = ["a".repeat(43), "b".repeat(43), "c".repeat(43)];
  const principalResources = [];
  let principalRequest = 0;
  const principal = Principal.host({
    async fetch(_input, init) {
      principalResources.push(JSON.parse(init.body).resources);
      return Response.json({
        token: tokens[principalRequest++],
        expires_at: Math.floor(Date.now() / 1_000) + 60,
      });
    },
  });
  const requests = [];
  let reconnectPrincipalId = principalId;
  const transport = Transport.from({
    key: "host-reconnect",
    name: "host-reconnect",
    type: "host-reconnect",
    setup() {
      return {
        baseUrl: "https://connect.example",
        async request(request) {
          requests.push(request);
          return hostConnectionWire(reconnectPrincipalId);
        },
      };
    },
  });
  const first = Client.create({
    appId,
    appOrigin,
    dialog: Dialog.memory(),
    principal,
    provider: {
      async request() {
        return { accounts: [{
          principal: { kind: "host", id: principalId },
          capabilities: { auth: { approval_id: "approval-host" } },
        }] };
      },
    },
    session: storage,
    transport,
  });
  await first.connection.connect({ capabilities: { cloudAccounts: { github: true } } });

  const restored = Client.create({
    appId,
    appOrigin,
    dialog: Dialog.memory(),
    principal,
    provider: { request() { throw new Error("provider must not reopen on reconnect"); } },
    session: storage,
    transport,
  });
  const connection = await restored.connection.reconnect();
  assert.equal(connection.principal.id, principalId);
  assert.deepEqual(principalResources[1], principalResources[0]);
  assert.deepEqual(requests[1].body, { host_principal_exchange: tokens[1] });

  reconnectPrincipalId = "q".repeat(43);
  assert.equal(await restored.connection.reconnect(), undefined);
  assert.equal(restored._hasSession(), false);
  assert.deepEqual(requests[2].body, { host_principal_exchange: tokens[2] });
});

function hostConnectionWire(id = principalId) {
  return {
    grant_token: "host-grant-token",
    principal: { kind: "host", id },
    agent_id: "agent_host",
    grant: {
      id: `0x${"33".repeat(32)}`,
      permission: "agent.run",
      status: "active",
      expires_at: Math.floor(Date.now() / 1_000) + 3_600,
      capabilities: ["nanocodex.agent", "agent.output.final", "agent.output.actions", "github"],
      mcp_connections: [],
    },
    authorization_mode: "hosted",
  };
}

function memoryStorage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: (key) => values.delete(key),
  };
}
