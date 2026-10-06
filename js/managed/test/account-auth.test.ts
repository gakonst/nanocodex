import { afterEach, describe, expect, it, vi } from "vitest";
import { env as workerEnv, runInDurableObject } from "cloudflare:test";

import {
  authenticate,
  forwardPrincipalAssertions,
  ensureAccount,
  ensureAccountWallet,
  resolveChiefOfStaffPrincipal,
  routeAccountRequest,
  type AccountAuthEnv,
} from "../src/account-auth";
import { routeConnectorRequest } from "../src/connectors";
import { routeCredentialRequest } from "../src/credentials";

const USER_ID = "11111111-1111-4111-8111-111111111111";
const ORGANIZATION_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const TEAM_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const LOCAL_HMAC_KEY = "shared-local-development-hmac-key";
const CREDENTIAL_ID = "cG9ydGFibGUtY3JlZGVudGlhbA";
const SECOND_USER_ID = "22222222-2222-4222-8222-222222222222";
const SECOND_CREDENTIAL_ID = "c2Vjb25kLXBvcnRhYmxlLWNyZWRlbnRpYWw";
const PUBLIC_KEY = "0x01020304";
const SECOND_PUBLIC_KEY = "0x05060708";
const LOCAL_PASSKEY_COOKIE = "nanocodex_local_passkey";
const CONNECT_GRANT_ID = `0x${"a".repeat(64)}`;
const CONNECT_MCP_ID = "m".repeat(43);
const CONNECTOR_CONNECTION_ID = "n".repeat(43);
const APP_TOOL_CATALOG_DIGEST = `0x${"c".repeat(64)}`;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("API key live authorization", () => {
  async function fixture(capabilities = ["agents:read"]) {
    const { env } = portableEnv();
    const token = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
    const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest(
      "SHA-256", new TextEncoder().encode(token),
    )))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    const key = {
      id: "a".repeat(12), label: "test", prefix: `ncx_live_${"a".repeat(12)}`,
      createdAt: 1, digest, userId: USER_ID, organizationId: ORGANIZATION_ID,
      teamId: TEAM_ID, role: "writer", authorizationEpoch: 1, capabilities,
    };
    let currentKey: unknown = key;
    let currentAccount: unknown = account(USER_ID, true);
    let grant: unknown = {
      organizationId: ORGANIZATION_ID, teamId: TEAM_ID, role: "owner",
      authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
    };
    let releaseAccount!: () => void;
    const accountGate = new Promise<void>((resolve) => { releaseAccount = resolve; });
    const keyFetch = vi.fn(async () => currentKey ? Response.json(currentKey) : new Response(null, { status: 404 }));
    const userFetch = vi.fn(async () => {
      await accountGate;
      return currentAccount ? Response.json(currentAccount) : new Response(null, { status: 404 });
    });
    const orgFetch = vi.fn(async () => grant ? Response.json(grant) : new Response(null, { status: 404 }));
    const namespace = (fetch: () => Promise<Response>) => ({ getByName: () => ({ fetch }) }) as unknown as DurableObjectNamespace;
    const testEnv = { ...env, NANOCODEX_API_KEYS: namespace(keyFetch), NANOCODEX_USERS: namespace(userFetch), NANOCODEX_ORGANIZATIONS: namespace(orgFetch) } as unknown as AccountAuthEnv;
    return {
      env: testEnv, token,
      authenticate: () => authenticate(new Request("https://example.com/v1/agents", { headers: { authorization: `Bearer ${token}` } }), testEnv),
      releaseAccount: () => releaseAccount(), userFetch, orgFetch,
      setKey: (value: unknown) => { currentKey = value; },
      setAccount: (value: unknown) => { currentAccount = value; },
      setGrant: (value: unknown) => { grant = value; },
    };
  }

  it("allows native owner keys to save Vault entries and approve origins, denying read-only or ephemeral accounts", async () => {
    const f = await fixture(["agents:read", "agents:write", "tools:use"]);
    f.releaseAccount();
    const binding = { fetch: vi.fn(async () => Response.json({ id: "v".repeat(32), kind: "login", name: "Example", created_at: 1 }, { status: 201 })) } as unknown as Fetcher;
    const env = { ...f.env, NANOCODEX: binding };
    const send = (path: string, method: string, body: unknown) => {
      const url = new URL(path, "https://nanocodex.example");
      return routeCredentialRequest(new Request(url, { method, headers: {
        authorization: `Bearer ${f.token}`, "content-type": "application/json",
      }, body: JSON.stringify(body) }), env, url);
    };
    expect((await send("/v1/credentials/vault/login", "POST", { name: "Example", username: "person", password: "fixture-password", browser_origin: "https://example.com" }))?.status).toBe(201);
    expect((await send(`/v1/credentials/vault/login/${"v".repeat(32)}/origin`, "PUT", { browser_origin: "https://example.com" }))?.status).toBe(201);
    expect((await send(`/v1/credentials/vault/login/${"v".repeat(32)}/origin`, "PUT", { browser_origin: "https://example.com/path" }))?.status).toBe(400);
    f.setAccount(account(USER_ID, false));
    expect((await send("/v1/credentials/vault/login", "POST", { name: "Example", username: "person", password: "fixture-password" }))?.status).toBe(401);
    expect(binding.fetch).toHaveBeenCalledTimes(2);
    const readonly = await fixture(); readonly.releaseAccount();
    const url = new URL("https://nanocodex.example/v1/credentials/vault/login");
    expect((await routeCredentialRequest(new Request(url, { method: "POST", headers: {
      authorization: `Bearer ${readonly.token}`, "content-type": "application/json",
    }, body: JSON.stringify({ name: "Example", username: "person", password: "fixture-password" }) }), { ...readonly.env, NANOCODEX: binding }, url))?.status).toBe(401);
  });

  it("starts membership resolution while the live account check is pending", async () => {
    const f = await fixture();
    const pending = f.authenticate();
    await vi.waitFor(() => expect(f.orgFetch).toHaveBeenCalledTimes(1));
    expect(f.userFetch).toHaveBeenCalledTimes(1);
    f.releaseAccount();
    expect(await pending).toMatchObject({ kind: "api_key", role: "writer", capabilities: ["agents:read"] });
  });

  it("checks key revocation and membership changes on every request", async () => {
    const f = await fixture();
    f.releaseAccount();
    expect(await f.authenticate()).toBeDefined();
    for (const change of [
      { authorizationEpoch: 2 }, { teamId: SECOND_USER_ID }, { organizationId: SECOND_USER_ID },
      { role: "reader" }, { capabilities: [] },
    ]) {
      f.setGrant({ organizationId: ORGANIZATION_ID, teamId: TEAM_ID, role: "owner", authorizationEpoch: 1, capabilities: ["agents:read"], ...change });
      expect(await f.authenticate()).toBeUndefined();
    }
    f.setKey(undefined);
    expect(await f.authenticate()).toBeUndefined();
  });

  it("rejects missing or mismatched accounts even when the parallel grant succeeds", async () => {
    const f = await fixture();
    f.releaseAccount();
    for (const current of [undefined, account(SECOND_USER_ID, true), { ...account(USER_ID, true), organizationId: SECOND_USER_ID }]) {
      f.setAccount(current);
      expect(await f.authenticate()).toBeUndefined();
    }
  });
});

describe("account-owned live authorization", () => {
  it("observes membership revocation and epoch changes without caching grants", async () => {
    const env = workerEnv as unknown as AccountAuthEnv;
    const userId = crypto.randomUUID();
    await ensureAccount(env, userId, true);
    const user = env.NANOCODEX_USERS.getByName(userId);
    const read = () => user.fetch("https://user.internal/authorization");
    const first = await (await read()).json<{ userId: string; grant: { organizationId: string; authorizationEpoch: number } }>();
    expect(first.userId).toBe(userId);
    expect(first.grant.authorizationEpoch).toBe(1);
    const organization = env.NANOCODEX_ORGANIZATIONS.getByName(first.grant.organizationId);
    await runInDurableObject(organization, async (_instance, state) => {
      const metadata = await state.storage.get<Record<string, unknown>>("metadata");
      await state.storage.put("metadata", { ...metadata, authorizationEpoch: 2 });
    });
    expect(await (await read()).json()).toMatchObject({ grant: { authorizationEpoch: 2 } });
    await runInDurableObject(organization, async (_instance, state) => {
      await state.storage.delete(`membership:user:${userId}`);
    });
    expect((await read()).status).toBe(404);
  });

  it("rejects absent accounts and wrong-user authorization responses", async () => {
    const env = workerEnv as unknown as AccountAuthEnv;
    const missing = env.NANOCODEX_USERS.getByName(crypto.randomUUID());
    expect((await missing.fetch("https://user.internal/authorization")).status).toBe(404);
    const local = portableEnv();
    const otherUser = local.env.NANOCODEX_USERS.getByName(SECOND_USER_ID);
    local.env.NANOCODEX_USERS = { getByName: () => otherUser } as unknown as AccountAuthEnv["NANOCODEX_USERS"];
    expect(await resolveChiefOfStaffPrincipal(local.env, USER_ID, `chief:${"a".repeat(64)}`)).toBeUndefined();
  });
});

describe("Connect grant assertions", () => {
  it("projects the trusted assertion to the exact live capability and tool slice", async () => {
    const { env } = portableEnv();
    const principal = await authenticate(new Request("https://nanocodex.internal/v1/agents", {
      headers: connectHeaders({
        capabilities: ["agents:read", "agents:write", "tools:use", "memory:read"],
        connectors: ["github", "gcalendar", "slack", "chatgpt"],
        connectorConnections: {
          github: [CONNECTOR_CONNECTION_ID],
          gcalendar: [CONNECTOR_CONNECTION_ID],
          slack: [CONNECTOR_CONNECTION_ID],
        },
        mcpIds: [CONNECT_MCP_ID],
        appToolCatalogDigest: APP_TOOL_CATALOG_DIGEST,
      }),
    }), env);

    expect(principal).toMatchObject({
      kind: "connect_grant",
      credentialId: CONNECT_GRANT_ID,
      capabilities: ["agents:read", "agents:write", "tools:use", "memory:read"],
      connectGrant: {
        grantId: CONNECT_GRANT_ID,
        connectors: ["github", "gcalendar", "slack", "chatgpt"],
        connectorConnections: {
          github: [CONNECTOR_CONNECTION_ID],
          gcalendar: [CONNECTOR_CONNECTION_ID],
          slack: [CONNECTOR_CONNECTION_ID],
        },
        mcpIds: [CONNECT_MCP_ID],
        appToolCatalogDigest: APP_TOOL_CATALOG_DIGEST,
      },
    });
  });

  it("accepts only a trusted exact sandbox assertion and strips client assertions on forwarding", async () => {
    const { env } = portableEnv();
    const headers = connectHeaders({});
    const read = (origin = "https://nanocodex.internal") => authenticate(new Request(`${origin}/v1/agents`, { headers }), env);
    expect((await read())?.connectGrant).not.toHaveProperty("sandboxExecution");
    headers.set("x-nanocodex-connect-sandbox-execution", "true");
    const principal = await read();
    expect(principal?.connectGrant).toMatchObject({ sandboxExecution: true });
    expect(await read("https://public.example")).toBeUndefined();
    const forwarded = new Headers({ "x-nanocodex-connect-sandbox-execution": "forged" });
    forwardPrincipalAssertions(forwarded, principal!);
    expect(forwarded.get("x-nanocodex-connect-sandbox-execution")).toBe("true");
    forwardPrincipalAssertions(forwarded, { ...principal!, connectGrant: undefined });
    expect(forwarded.has("x-nanocodex-connect-sandbox-execution")).toBe(false);
    for (const invalid of ["false", "1", "true, true", "agent.execution.sandbox", ""]) {
      headers.set("x-nanocodex-connect-sandbox-execution", invalid);
      expect(await read()).toBeUndefined();
    }
  });

  it("rejects incomplete, malformed, duplicate, or account-widening assertions", async () => {
    const { env } = portableEnv();
    const request = (headers: HeadersInit) => authenticate(new Request(
      "https://nanocodex.internal/v1/agents",
      { headers },
    ), env);

    await expect(request({ "x-nanocodex-connect-user": USER_ID })).resolves.toBeUndefined();
    await expect(request(connectHeaders({ connectors: ["github", "github"] })))
      .resolves.toBeUndefined();
    await expect(request(connectHeaders({ capabilities: ["organization:write"] })))
      .resolves.toBeUndefined();
    await expect(request(connectHeaders({ mcpIds: ["short"] })))
      .resolves.toBeUndefined();
    await expect(request(connectHeaders({
      connectors: ["github"],
      connectorConnections: { github: ["short"] },
    }))).resolves.toBeUndefined();
    await expect(request(connectHeaders({
      connectors: ["github"],
      connectorConnections: { slack: [CONNECTOR_CONNECTION_ID] },
    }))).resolves.toBeUndefined();
    await expect(request(connectHeaders({ appToolCatalogDigest: "not-a-digest" })))
      .resolves.toBeUndefined();
    const duplicateCatalogDigest = connectHeaders({ appToolCatalogDigest: APP_TOOL_CATALOG_DIGEST });
    duplicateCatalogDigest.append(
      "x-nanocodex-connect-app-tool-catalog-digest",
      APP_TOOL_CATALOG_DIGEST,
    );
    await expect(request(duplicateCatalogDigest)).resolves.toBeUndefined();
  });
});

describe("connector route compatibility", () => {
  it("lets owner device keys list and manage connectors without granting agent keys account control", async () => {
    const local = portableEnv();
    const token = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
    const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest(
      "SHA-256", new TextEncoder().encode(token),
    )))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    const key = {
      id: "a".repeat(12), label: "iPhone", prefix: `ncx_live_${"a".repeat(12)}`,
      createdAt: 1, digest, userId: USER_ID, organizationId: ORGANIZATION_ID,
      teamId: TEAM_ID, role: "owner", authorizationEpoch: 1,
      capabilities: ["api_keys:write", "tools:use"],
    };
    let currentKey: unknown = key;
    const seen: Request[] = [];
    const env = { ...local.env,
      NANOCODEX_API_KEYS: { getByName: () => ({ async fetch() {
        return currentKey ? Response.json(currentKey) : new Response(null, { status: 404 });
      } }) } as unknown as AccountAuthEnv["NANOCODEX_API_KEYS"],
      NANOCODEX: { async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init); seen.push(request);
        return request.method === "DELETE" ? new Response(null, { status: 204 })
          : Response.json({ connectors: {}, mcp_connections: [] });
      } } as Fetcher,
    };
    const routes = [
      ["GET", "/v1/connectors/catalog", 200],
      ["GET", "/v1/connectors", 200],
      ["GET", "/v1/connectors/mcp-connections", 200],
      ["DELETE", `/v1/connectors/spotify/connections/${CONNECTOR_CONNECTION_ID}`, 204],
      ["DELETE", `/v1/connectors/mcp-connections/${CONNECT_MCP_ID}`, 204],
    ] as const;
    for (const [method, path, status] of routes) {
      const url = new URL(path, "https://nanocodex.example");
      const call = () => routeConnectorRequest(new Request(url, {
        method, headers: { authorization: `Bearer ${token}` },
      }), env, url);
      currentKey = key;
      expect((await call())?.status, path).toBe(status);
      for (const denied of [null, { ...key, role: "writer" }, { ...key, capabilities: ["tools:use"] }]) {
        currentKey = denied;
        const prior = seen.length;
        expect((await call())?.status, path).toBe(401);
        expect(seen).toHaveLength(prior);
      }
    }
    expect(seen.every(request => new URL(request.url).pathname.startsWith(`/users/${USER_ID}/`))).toBe(true);
  });

  it.each(["spotify", "soundcloud"])("keeps %s phone callbacks owner-bound and stamps the provider flow", async (provider) => {
    const local = portableEnv();
    const cookie = persistentAccountCookie(local, USER_ID, "d");
    const requests: Request[] = [];
    const env = { ...local.env, NANOCODEX: {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        requests.push(new Request(input, init));
        return Response.json({ connected: true });
      },
    } as Fetcher };
    const origin = "https://nanocodex.example";
    const url = new URL(`/v1/connectors/${provider}/loopback/callback`, origin);
    const body = JSON.stringify({ state: "s".repeat(43), code: "one-time-code" });
    const call = (headers: Record<string, string>, payload = body) => routeConnectorRequest(new Request(url, {
      method: "POST", headers: { "content-type": "application/json", ...headers }, body: payload,
    }), env, url);
    expect((await call({ origin }))?.status).toBe(401);
    expect((await call({ cookie, origin: "https://evil.test" }))?.status).toBe(403);
    expect((await call({ cookie, origin }, JSON.stringify({ ...JSON.parse(body), client_secret: "secret" })))?.status).toBe(400);
    expect(requests).toHaveLength(0);
    expect((await call({ cookie, origin }))?.status).toBe(200);
    expect(requests).toHaveLength(1);
    expect(new URL(requests[0]!.url).pathname).toBe(`/users/${USER_ID}/connectors/${provider}/callback`);
    expect(await requests[0]!.json()).toEqual({ ...JSON.parse(body), flow: provider === "spotify" ? "ncspot_loopback" : "soundcloud_loopback" });
  });

  it("finishes an OAuth MCP in the native app without returning OAuth material", async () => {
    const local = portableEnv();
    const sessionToken = "e".repeat(64);
    const attempt = "6f3eec23-8a1a-4de4-b498-1689a2829ca0";
    local.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const requests: Request[] = [];
    const env = {
      ...local.env,
      NANOCODEX: {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          requests.push(new Request(input, init));
          return Response.json({
            return_to: `/v1/connectors/mcp-mobile-complete?attempt=${attempt}`,
            mcp_connections: [{ id: CONNECT_MCP_ID, name: "Linear", status: "connected" }],
          });
        },
      } as unknown as Fetcher,
    };
    const callbackUrl = new URL(
      `https://nanocodex.example/v1/connectors/mcp-connections/${CONNECT_MCP_ID}/callback?code=private-code&state=private-state`,
    );
    const callback = await routeConnectorRequest(new Request(callbackUrl, {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), env, callbackUrl);

    expect(callback?.status).toBe(303);
    const completionUrl = new URL(callback!.headers.get("location")!);
    expect(completionUrl.pathname).toBe("/v1/connectors/mcp-mobile-complete");
    expect(completionUrl.searchParams.get("mcp_result")).toBe("connected");
    expect(completionUrl.href).not.toMatch(/private-code|private-state/);
    const native = await routeConnectorRequest(new Request(completionUrl), env, completionUrl);
    expect(native?.status).toBe(303);
    expect(native?.headers.get("location")).toBe(
      `nanocodex://connectors/mcp-complete?attempt=${attempt}&mcp_connection=${CONNECT_MCP_ID}&mcp_result=connected`,
    );
    expect(requests).toHaveLength(1);
  });
});

describe("account provisioning", () => {
  it("narrows Chief of Staff principals to the agent tool boundary", async () => {
    const { env } = portableEnv();
    const principal = await resolveChiefOfStaffPrincipal(env, USER_ID, `chief:${"a".repeat(64)}`);

    expect(principal).toMatchObject({
      kind: "service",
      userId: USER_ID,
      capabilities: ["agents:read", "agents:write", "tools:use"],
    });
    expect(principal?.capabilities).not.toContain("organization:write");
    expect(principal?.capabilities).not.toContain("api_keys:write");
    expect(principal?.capabilities).not.toContain("agents:portability");
  });

  it("accepts a matching persistent account after a create conflict", async () => {
    const requests: string[] = [];
    const env = accountEnv(async (request) => {
      requests.push(request.method);
      return request.method === "PUT"
        ? new Response(null, { status: 409 })
        : Response.json(account(USER_ID, true));
    });

    await expect(ensureAccount(env, USER_ID, true)).resolves.toBeUndefined();
    expect(requests).toEqual(["PUT", "GET"]);
  });

  it("rejects a conflict owned by another account", async () => {
    const env = accountEnv(async (request) => request.method === "PUT"
      ? new Response(null, { status: 409 })
      : Response.json(account("22222222-2222-4222-8222-222222222222", true)));

    await expect(ensureAccount(env, USER_ID, true)).rejects.toThrow("account provisioning failed");
  });

  it("does not promote an anonymous account without a successful write", async () => {
    const env = accountEnv(async (request) => request.method === "PUT"
      ? new Response(null, { status: 409 })
      : Response.json(account(USER_ID, false)));

    await expect(ensureAccount(env, USER_ID, true)).rejects.toThrow("account provisioning failed");
  });
});

describe("managed wallet bridge", () => {
  it("bounds broker wallet provisioning so OTP verification cannot hang forever", async () => {
    const local = portableEnv();
    local.env.NANOCODEX = {
      fetch: () => new Promise<Response>(() => {}),
    } as unknown as Fetcher;

    await expect(ensureAccountWallet(local.env, USER_ID, 1)).rejects.toThrow("wallet unavailable");
  });

  it("requires a persistent session and same-origin mutations before forwarding wallet requests", async () => {
    const local = portableEnv();
    const requests: Request[] = [];
    local.env.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        requests.push(new Request(input, init));
        return Response.json({ accepted: true }, { status: 202, headers: { "x-wallet": "preserved" } });
      },
    } as Fetcher;
    const origin = "https://nanocodex.example";
    const url = new URL("/v1/wallet/connect", origin);
    const anonymous = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { "content-type": "application/json", origin },
      body: "{}",
    }), local.env, url);
    expect(anonymous?.status).toBe(401);

    const cookie = persistentAccountCookie(local, USER_ID, "d");
    const crossOrigin = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin: "https://evil.example" },
      body: "{}",
    }), local.env, url);
    expect(crossOrigin?.status).toBe(403);

    const rejectedMaterial = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin },
      body: JSON.stringify({ private_key: "0xdeadbeef" }),
    }), local.env, url);
    expect(rejectedMaterial?.status).toBe(400);
    expect(requests).toHaveLength(0);

    const publicScopeAddress = "0x3333333333333333333333333333333333333333";
    const allowedPublicAddress = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin },
      body: JSON.stringify({
        request: {
          method: "wallet_connect",
          params: [{
            capabilities: {
              authorizeAccessKey: {
                scopes: [{ address: publicScopeAddress, type: "contract" }],
              },
            },
          }],
        },
      }),
    }), local.env, url);
    expect(allowedPublicAddress?.status).toBe(202);
    expect(requests).toHaveLength(1);
    await expect(requests[0]!.json()).resolves.toMatchObject({
      request: {
        params: [{ capabilities: { authorizeAccessKey: { scopes: [{ address: publicScopeAddress }] } } }],
      },
    });

    const balanceUrl = new URL("/v1/wallet/balance", origin);
    const anonymousBalance = await routeAccountRequest(new Request(balanceUrl), local.env, balanceUrl);
    expect(anonymousBalance?.status).toBe(401);
    const balance = await routeAccountRequest(new Request(balanceUrl, {
      headers: { cookie },
    }), local.env, balanceUrl);
    expect(balance?.status).toBe(202);
    expect(requests[1]?.url).toBe(`https://broker.internal/users/${USER_ID}/wallet/balance`);
  });

  it("forwards each wallet operation only to its authenticated user and uses the canonical address for /v1/me", async () => {
    const local = portableEnv();
    const requests: Request[] = [];
    local.env.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        requests.push(request);
        return Response.json({
          address: new URL(request.url).pathname.includes(SECOND_USER_ID)
            ? "0x2222222222222222222222222222222222222222"
            : "0x1111111111111111111111111111111111111111",
          created_at: 1,
        });
      },
    } as Fetcher;
    const origin = "https://nanocodex.example";
    const firstCookie = persistentAccountCookie(local, USER_ID, "d");
    const secondCookie = persistentAccountCookie(local, SECOND_USER_ID, "e");
    local.set("account", `address:${USER_ID}`, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    const firstUrl = new URL("/v1/wallet/connect", origin);
    const first = await routeAccountRequest(new Request(firstUrl, {
      method: "POST",
      headers: { cookie: firstCookie, "content-type": "application/json", origin },
      body: JSON.stringify({ request: { method: "wallet_connect", params: [{}] } }),
    }), local.env, firstUrl);
    expect(first?.status).toBe(200);

    const secondUrl = new URL("/v1/wallet/revoke-access-key", origin);
    await routeAccountRequest(new Request(secondUrl, {
      method: "POST",
      headers: { cookie: secondCookie, "content-type": "application/json", origin },
      body: JSON.stringify({ request: { method: "wallet_revokeAccessKey", params: [{ key_id: "key_123" }] } }),
    }), local.env, secondUrl);
    const mutations = requests.filter((request) => request.method === "POST");
    expect(mutations.map((request) => request.url)).toEqual([
      `https://broker.internal/users/${USER_ID}/wallet/connect`,
      `https://broker.internal/users/${SECOND_USER_ID}/wallet/revoke-access-key`,
    ]);
    await expect(mutations[0]!.json()).resolves.toEqual({
      request: { method: "wallet_connect", params: [{}] },
    });
    await expect(mutations[1]!.json()).resolves.toEqual({
      request: {
        method: "wallet_revokeAccessKey",
        params: [{ address: "0x2222222222222222222222222222222222222222", key_id: "key_123" }],
      },
    });

    const meUrl = new URL("/v1/me", origin);
    const me = await routeAccountRequest(new Request(meUrl, { headers: { cookie: firstCookie } }), local.env, meUrl);
    await expect(me?.json()).resolves.toMatchObject({
      user: { address: "0x1111111111111111111111111111111111111111", id: USER_ID },
    });
  });
});

describe("local WebAuthn credential portability", () => {
  it("uses one parent RP ID while retaining exact per-request origin checks", async () => {
    for (const [origin, rpId] of [
      ["http://nanocodex.localhost:5173", "nanocodex.localhost"],
      ["http://branch.nanocodex.localhost", "nanocodex.localhost"],
      ["http://branch.nanocodex.localhost:20735", "nanocodex.localhost"],
      ["https://branch.nanocodex.localhost:20735", "nanocodex.localhost"],
      ["https://nanocodex.example", "nanocodex.example"],
      ["https://localhost", "localhost"],
      ["https://nanocodex.local", "nanocodex.local"],
      ["http://branch.example", "branch.example"],
    ]) {
      const { env } = portableEnv();
      const response = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
        method: "POST",
        headers: { "content-type": "application/json", origin },
        body: "{}",
      }), env, new URL(`${origin}/webauthn/login/options`));
      expect(response?.status).toBe(200);
      const body = await response!.json<{ options: { publicKey: { rpId: string } } }>();
      expect(body.options.publicKey.rpId).toBe(rpId);
    }

    const { env } = portableEnv();
    const rejected = await routeAccountRequest(new Request(
      "http://branch.nanocodex.localhost:20735/webauthn/login/options",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          origin: "http://branch.nanocodex.localhost:20736",
        },
        body: "{}",
      },
    ), env, new URL("http://branch.nanocodex.localhost:20735/webauthn/login/options"));
    expect(rejected?.status).toBe(403);
  });

  it("carries one signed credential into an isolated local auth store", async () => {
    const source = portableEnv();
    const sessionToken = "a".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });

    const migrated = await routeAccountRequest(new Request("http://one.nanocodex.localhost:20735/v1/me", {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL("http://one.nanocodex.localhost:20735/v1/me"));
    expect(migrated?.status).toBe(200);
    const setCookie = localPasskeySetCookie(migrated!.headers);
    expect(setCookie).toBeDefined();
    expect(setCookie).toContain("Domain=nanocodex.localhost");
    expect(setCookie).toContain("HttpOnly");
    expect(setCookie).toContain("SameSite=Lax");
    expect(setCookie).toContain("Secure");
    const portableCookie = setCookie!.split(";", 1)[0]!;
    const payload = portableCookie.split("=", 2)[1]!.split(".", 1)[0]!;
    expect(JSON.parse(decodeBase64Url(payload))).toEqual({
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: USER_ID,
    });

    const target = portableEnv();
    const options = await routeAccountRequest(new Request(
      "http://two.nanocodex.localhost:20736/webauthn/login/options",
      {
        method: "POST",
        headers: {
          cookie: portableCookie,
          "content-type": "application/json",
          origin: "http://two.nanocodex.localhost:20736",
        },
        body: JSON.stringify({ credentialId: CREDENTIAL_ID }),
      },
    ), target.env, new URL("http://two.nanocodex.localhost:20736/webauthn/login/options"));
    expect(await options!.json()).toMatchObject({
      options: {
        publicKey: {
          allowCredentials: [{ id: CREDENTIAL_ID }],
        },
      },
    });

    const attempted = await loginWithPortableCookie(target.env, portableCookie, CREDENTIAL_ID);
    expect(attempted.status).toBe(400);
    expect(target.get("webauthn", `credential:${CREDENTIAL_ID}`)).toEqual({
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
    });
  });

  it("targets the selected older account after logout while the portable hint is newer", async () => {
    const target = portableEnv();
    const latestSessionToken = "b".repeat(64);
    target.set("webauthn", `credential:${CREDENTIAL_ID}`, {
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
    });
    target.set("webauthn", `credential:${SECOND_CREDENTIAL_ID}`, {
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
    });
    target.set("webauthn", `session:${latestSessionToken}`, {
      credentialId: SECOND_CREDENTIAL_ID,
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const origin = "http://two.nanocodex.localhost:20736";
    const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
      headers: { cookie: `nanocodex_account=${latestSessionToken}` },
    }), target.env, new URL(`${origin}/v1/me`));
    const portableCookie = localPasskeySetCookie(current!.headers)!.split(";", 1)[0]!;

    const logout = await routeAccountRequest(new Request(`${origin}/webauthn/logout`, {
      method: "POST",
      headers: { cookie: `nanocodex_account=${latestSessionToken}`, origin },
    }), target.env, new URL(`${origin}/webauthn/logout`));
    expect(logout?.status).toBe(204);
    expect(target.get("webauthn", `session:${latestSessionToken}`)).toBeUndefined();

    const selected = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: JSON.stringify({ credentialId: CREDENTIAL_ID }),
    }), target.env, new URL(`${origin}/webauthn/login/options`));
    expect(selected?.status).toBe(200);
    const selectedBody = await selected!.json<{
      options: { publicKey: { allowCredentials: { id: string }[]; challenge: string } };
    }>();
    expect(selectedBody.options.publicKey.allowCredentials).toEqual([
      { id: CREDENTIAL_ID, type: "public-key" },
    ]);

    const mismatched = await routeAccountRequest(new Request(`${origin}/webauthn/login`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: JSON.stringify({
        id: SECOND_CREDENTIAL_ID,
        metadata: {
          clientDataJSON: JSON.stringify({ challenge: selectedBody.options.publicKey.challenge }),
        },
      }),
    }), target.env, new URL(`${origin}/webauthn/login`));
    expect(mismatched?.status).toBe(400);
    expect(await mismatched!.json()).toEqual({ error: "selected_credential_mismatch" });
  });

  it("refuses an unknown selected credential instead of falling back to the portable hint", async () => {
    const source = portableEnv();
    const sessionToken = "b".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: SECOND_CREDENTIAL_ID,
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const origin = "http://two.nanocodex.localhost:20736";
    const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL(`${origin}/v1/me`));
    const portableCookie = localPasskeySetCookie(current!.headers)!.split(";", 1)[0]!;
    const unknown = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: JSON.stringify({ credentialId: "unknown-saved-passkey" }),
    }), source.env, new URL(`${origin}/webauthn/login/options`));

    expect(unknown?.status).toBe(400);
    expect(await unknown!.json()).toEqual({ error: "unknown credential" });
  });

  it("leaves an untargeted login discoverable even when a portable hint exists", async () => {
    const source = portableEnv();
    const sessionToken = "d".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: SECOND_CREDENTIAL_ID,
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const origin = "http://two.nanocodex.localhost:20736";
    const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL(`${origin}/v1/me`));
    const portableCookie = localPasskeySetCookie(current!.headers)!.split(";", 1)[0]!;
    const chooser = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: "{}",
    }), source.env, new URL(`${origin}/webauthn/login/options`));
    const body = await chooser!.json<{ options: { publicKey: Record<string, unknown> } }>();

    expect(chooser?.status).toBe(200);
    expect(body.options.publicKey).not.toHaveProperty("allowCredentials");
  });

  it("does not import tampered, mismatched, unsigned, or differently signed records", async () => {
    const source = portableEnv();
    const sessionToken = "c".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request("http://one.nanocodex.localhost:20735/v1/me", {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL("http://one.nanocodex.localhost:20735/v1/me"));
    const portableCookie = localPasskeySetCookie(migrated!.headers)!.split(";", 1)[0]!;
    const [name, value] = portableCookie.split("=", 2) as [string, string];
    const [payload, signature] = value.split(".") as [string, string];
    const tamperedSignature = `${signature[0] === "A" ? "B" : "A"}${signature.slice(1)}`;

    for (const testCase of [
      { cookie: `${name}=${payload}.${tamperedSignature}`, id: CREDENTIAL_ID },
      { cookie: portableCookie, id: "different-credential" },
      { cookie: `${name}=${payload}`, id: CREDENTIAL_ID },
    ]) {
      const target = portableEnv();
      await loginWithPortableCookie(target.env, testCase.cookie, testCase.id);
      expect(target.get("webauthn", `credential:${CREDENTIAL_ID}`)).toBeUndefined();
      expect(target.get("webauthn", `credential:${testCase.id}`)).toBeUndefined();
    }

    const differentKey = portableEnv("different-local-hmac-key");
    await loginWithPortableCookie(differentKey.env, portableCookie, CREDENTIAL_ID);
    expect(differentKey.get("webauthn", `credential:${CREDENTIAL_ID}`)).toBeUndefined();
  });

  it("issues the same portable record on the browser-safe localhost fallback", async () => {
    const source = portableEnv();
    const sessionToken = "e".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request(
      "http://passkey-a.nanocodex.localhost:20735/v1/me",
      { headers: { cookie: `nanocodex_account=${sessionToken}` } },
    ), source.env, new URL("http://passkey-a.nanocodex.localhost:20735/v1/me"));
    const setCookie = localPasskeySetCookie(migrated!.headers);
    expect(setCookie).toBeDefined();
    expect(setCookie).toContain("Domain=nanocodex.localhost");
    expect(setCookie).toContain("Secure");

    const target = portableEnv();
    await loginWithPortableCookie(
      target.env,
      setCookie!.split(";", 1)[0]!,
      CREDENTIAL_ID,
      "http://passkey-b.nanocodex.localhost:20736",
    );
    expect(target.get("webauthn", `credential:${CREDENTIAL_ID}`)).toEqual({
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
    });
  });

  it("lets an exact localhost origin forget only its portable credential hint", async () => {
    const { env } = portableEnv();
    const origin = "http://passkey-a.nanocodex.localhost:20735";
    const response = await routeAccountRequest(new Request(
      `${origin}/webauthn/portable-credential`,
      { method: "DELETE", headers: { origin } },
    ), env, new URL(`${origin}/webauthn/portable-credential`));
    expect(response?.status).toBe(204);
    expect(localPasskeySetCookie(response!.headers)).toContain(
      "nanocodex_local_passkey=; Path=/; Domain=nanocodex.localhost; Max-Age=0",
    );

    const wrongOrigin = await routeAccountRequest(new Request(
      `${origin}/webauthn/portable-credential`,
      { method: "DELETE", headers: { origin: "http://passkey-b.nanocodex.localhost:20736" } },
    ), env, new URL(`${origin}/webauthn/portable-credential`));
    expect(wrongOrigin?.status).toBe(403);
  });

  it("never issues or imports the portable record on production or generic loopback origins", async () => {
    const source = portableEnv();
    const portableSessionToken = "c".repeat(64);
    source.set("webauthn", `session:${portableSessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request("http://nanocodex.localhost:5173/v1/me", {
      headers: { cookie: `nanocodex_account=${portableSessionToken}` },
    }), source.env, new URL("http://nanocodex.localhost:5173/v1/me"));
    const portableCookie = localPasskeySetCookie(migrated!.headers)!.split(";", 1)[0]!;

    for (const origin of [
      "https://nanocodex.example",
      "https://localhost",
      "https://127.0.0.1",
      "http://branch.example",
      "http://nanocodex.local",
      "http://nested.branch.nanocodex.localhost:20735",
    ]) {
      const local = portableEnv();
      const sessionToken = "f".repeat(64);
      local.set("webauthn", `session:${sessionToken}`, {
        credentialId: CREDENTIAL_ID,
        publicKey: PUBLIC_KEY,
        userId: encodeUserId(USER_ID),
        issuedAt: 1,
        expiresAt: Math.floor(Date.now() / 1_000) + 60,
      });
      const response = await routeAccountRequest(new Request(`${origin}/v1/me`, {
        headers: { cookie: `nanocodex_account=${sessionToken}` },
      }), local.env, new URL(`${origin}/v1/me`));
      expect(response?.status).toBe(200);
      expect(localPasskeySetCookie(response!.headers)).toBeUndefined();

      await loginWithPortableCookie(local.env, portableCookie, CREDENTIAL_ID, origin);
      expect(local.get("webauthn", `credential:${CREDENTIAL_ID}`)).toBeUndefined();
    }
  });
});

describe("manual credential vault account boundary", () => {
  it.each(["login", "api_key"])("requires a persistent same-origin session and forwards only validated JSON (%s)", async (kind) => {
    const payload = kind === "api_key" ? { name: "Example", api_key: "secret" }
      : { name: "Example", username: "person", password: "secret" };
    const local = portableEnv();
    const sessionToken = "9".repeat(64);
    local.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const seen: Request[] = [];
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        seen.push(request);
        return request.method === "POST"
          ? Response.json({ id: "v".repeat(32), kind, name: "Example", created_at: 1 }, {
              status: 201,
            })
          : new Response(null, { status: 204 });
      },
    } as Fetcher;
    const credentialEnv = { ...local.env, NANOCODEX: binding };
    const origin = "http://nanocodex.localhost:20735";
    const url = new URL(`/v1/credentials/vault/${kind}`, origin);
    const headers = {
      cookie: `nanocodex_account=${sessionToken}`,
      "content-type": "application/json; charset=utf-8",
      origin,
    };
    const created = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers,
      body: JSON.stringify(payload),
    }), credentialEnv, url);
    expect(created?.status).toBe(201);
    expect(seen).toHaveLength(1);
    expect(seen[0]!.url).toBe(
      `https://broker.internal/users/${USER_ID}/credentials/vault/${kind}`,
    );
    expect(seen[0]!.headers.get("content-type")).toBe("application/json");
    expect(await seen[0]!.json()).toEqual(payload);

    const unauthenticated = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: { "content-type": "application/json", origin },
      body: JSON.stringify(payload),
    }), credentialEnv, url);
    expect(unauthenticated?.status).toBe(401);

    const crossOrigin = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: { ...headers, origin: "https://attacker.example" },
      body: JSON.stringify(payload),
    }), credentialEnv, url);
    expect(crossOrigin?.status).toBe(403);
    expect(seen).toHaveLength(1);

    const deleteUrl = new URL(`/v1/credentials/vault/${kind}/${"v".repeat(32)}`, origin);
    const removed = await routeCredentialRequest(new Request(deleteUrl, {
      method: "DELETE",
      headers: { cookie: headers.cookie, origin },
    }), credentialEnv, deleteUrl);
    expect(removed?.status).toBe(204);
    expect(seen[1]!.method).toBe("DELETE");
    expect(seen[1]!.body).toBeNull();
  });

  it("rejects invalid content, schemas, and oversized bodies before egress", async () => {
    const local = portableEnv();
    const sessionToken = "8".repeat(64);
    local.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    const credentialEnv = { ...local.env, NANOCODEX: binding };
    const origin = "http://nanocodex.localhost:20735";
    const url = new URL("/v1/credentials/vault/card", origin);
    const baseHeaders = { cookie: `nanocodex_account=${sessionToken}`, origin };

    const wrongType = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: baseHeaders,
      body: "{}",
    }), credentialEnv, url);
    expect(wrongType?.status).toBe(415);

    const invalid = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: { ...baseHeaders, "content-type": "application/json" },
      body: JSON.stringify({
        name: "Card",
        card_number: "4111111111111111",
        expiry_month: 9,
        expiry_year: "2031",
        cvv: "123",
        billing_zip: "10001",
      }),
    }), credentialEnv, url);
    expect(invalid?.status).toBe(400);

    const oversized = await routeCredentialRequest(new Request(
      new URL("/v1/credentials/vault/login", origin),
      {
        method: "POST",
        headers: { ...baseHeaders, "content-type": "application/json" },
        body: JSON.stringify({
          name: "Example",
          username: "person",
          password: "x".repeat(13 * 1024),
        }),
      },
    ), credentialEnv, new URL("/v1/credentials/vault/login", origin));
    expect(oversized?.status).toBe(413);
    expect(binding.fetch).not.toHaveBeenCalled();
  });
});

function accountEnv(fetch: (request: Request) => Promise<Response>): AccountAuthEnv {
  return {
    NANOCODEX_USERS: {
      getByName() {
        return {
          fetch(input: RequestInfo | URL, init?: RequestInit) {
            return fetch(new Request(input, init));
          },
        };
      },
    },
  } as unknown as AccountAuthEnv;
}

function connectHeaders(overrides: Readonly<{
  appToolCatalogDigest?: string;
  capabilities?: readonly string[];
  connectors?: readonly string[];
  connectorConnections?: Readonly<Record<string, readonly string[]>>;
  mcpIds?: readonly string[];
}> = {}): Headers {
  return new Headers({
    "x-nanocodex-connect-user": USER_ID,
    "x-nanocodex-connect-grant-id": CONNECT_GRANT_ID,
    "x-nanocodex-connect-capabilities": JSON.stringify(
      overrides.capabilities ?? ["agents:read", "agents:write", "tools:use"],
    ),
    "x-nanocodex-connect-connectors": JSON.stringify(overrides.connectors ?? []),
    "x-nanocodex-connect-mcp-ids": JSON.stringify(overrides.mcpIds ?? []),
    ...(overrides.appToolCatalogDigest === undefined
      ? {}
      : {
        "x-nanocodex-connect-app-tool-catalog-digest": overrides.appToolCatalogDigest,
      }),
    ...(overrides.connectorConnections === undefined
      ? {}
      : {
        "x-nanocodex-connect-connector-connections": JSON.stringify(
          overrides.connectorConnections,
        ),
      }),
  });
}

function localPasskeySetCookie(headers: Headers): string | undefined {
  return headers.getSetCookie().find((cookie) => cookie.startsWith(`${LOCAL_PASSKEY_COOKIE}=`));
}

function portableEnv(secret = LOCAL_HMAC_KEY): {
  env: AccountAuthEnv;
  get(name: string, key: string): unknown;
  set(name: string, key: string, value: unknown): void;
  values(name: string): IterableIterator<unknown>;
} {
  const stores = new Map<string, Map<string, unknown>>();
  const store = (name: string) => {
    let current = stores.get(name);
    if (!current) {
      current = new Map();
      stores.set(name, current);
    }
    return current;
  };
  const auth = {
    idFromName(name: string) {
      return name;
    },
    get(id: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          const url = new URL(request.url);
          const key = url.searchParams.get("key")!;
          const current = store(id);
          if (url.pathname === "/get") return Response.json({ value: current.get(key) });
          if (url.pathname === "/set") {
            const body = await request.json<{ value: unknown }>();
            current.set(key, body.value);
            return Response.json({ ok: true });
          }
          if (url.pathname === "/create") {
            const body = await request.json<{ value: unknown }>();
            const created = !current.has(key);
            if (created) current.set(key, body.value);
            return Response.json({ created });
          }
          if (url.pathname === "/delete") {
            current.delete(key);
            return Response.json({ ok: true });
          }
          if (url.pathname === "/take") {
            const value = current.get(key);
            current.delete(key);
            return Response.json({ value });
          }
          return new Response(null, { status: 404 });
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const users = {
    getByName(userId: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          if (new URL(request.url).pathname === "/authorization") {
            const grant = await (await organizations.getByName(ORGANIZATION_ID).fetch("https://organization.internal/resolve")).json();
            return Response.json({ userId, grant });
          }
          if (new URL(request.url).pathname !== "/account") {
            return new Response(null, { status: 404 });
          }
          if (request.method === "PUT") {
            const body = await request.json<{ persistent: boolean }>();
            return Response.json(account(userId, body.persistent));
          }
          return Response.json(account(userId, true));
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const organizations = {
    getByName() {
      return {
        fetch() {
          return Promise.resolve(Response.json({
            organizationId: ORGANIZATION_ID,
            teamId: TEAM_ID,
            role: "owner",
            authorizationEpoch: 1,
            capabilities: [
              "agents:read",
              "agents:write",
              "api_keys:read",
              "api_keys:write",
              "history:read",
              "memory:read",
              "memory:write",
              "tools:use",
              "organization:read",
              "organization:write",
            ],
          }));
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  return {
    env: {
      NANOCODEX_AUTH: auth,
      NANOCODEX: {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          const match = new URL(request.url).pathname.match(
            /^\/users\/([0-9a-f-]+)\/wallet(?:\/(connect|revoke-access-key))?$/,
          );
          if (!match) return new Response(null, { status: 404 });
          return Response.json({
            address: "0x1111111111111111111111111111111111111111",
            created_at: 1,
          });
        },
      } as Fetcher,
      NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: secret,
      NANOCODEX_ORGANIZATIONS: organizations,
      NANOCODEX_USERS: users,
    } as unknown as AccountAuthEnv,
    get: (name, key) => store(name).get(key),
    set: (name, key, value) => store(name).set(key, value),
    values: (name) => store(name).values(),
  };
}

function loginWithPortableCookie(
  env: AccountAuthEnv,
  cookie: string,
  credentialId: string,
  origin = "http://two.nanocodex.localhost:20736",
): Promise<Response> {
  const url = new URL("/webauthn/login", origin);
  return routeAccountRequest(new Request(url, {
    method: "POST",
    headers: {
      cookie,
      "content-type": "application/json",
      origin: url.origin,
    },
    body: JSON.stringify({
      id: credentialId,
      metadata: { clientDataJSON: JSON.stringify({ challenge: "AQ" }) },
    }),
  }), env, url) as Promise<Response>;
}

function persistentAccountCookie(
  local: ReturnType<typeof portableEnv>,
  userId: string,
  character: string,
): string {
  const token = character.repeat(64);
  local.set("webauthn", `session:${token}`, {
    credentialId: CREDENTIAL_ID,
    publicKey: PUBLIC_KEY,
    userId: encodeUserId(userId),
    issuedAt: 1,
    expiresAt: Math.floor(Date.now() / 1_000) + 60,
  });
  return `nanocodex_account=${token}`;
}

function encodeUserId(value: string): string {
  return btoa(value).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeBase64Url(value: string): string {
  const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
  return atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "="));
}

function account(id: string, persistent: boolean) {
  return {
    id,
    organizationId: ORGANIZATION_ID,
    persistent,
    createdAt: 1,
    lastAuthenticatedAt: 1,
  };
}
