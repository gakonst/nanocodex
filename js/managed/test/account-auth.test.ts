import { describe, expect, it } from "vitest";

import {
  authenticate,
  ensureAccount,
  routeAccountRequest,
  type AccountAuthEnv,
} from "../src/account-auth";

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

describe("account provisioning", () => {
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
      NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: secret,
      NANOCODEX_ORGANIZATIONS: organizations,
      NANOCODEX_USERS: users,
    } as unknown as AccountAuthEnv,
    get: (name, key) => store(name).get(key),
    set: (name, key, value) => store(name).set(key, value),
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
