import { describe, expect, it } from "vitest";

import {
  ensureAccount,
  routeAccountRequest,
  type AccountAuthEnv,
} from "../src/account-auth";

const USER_ID = "11111111-1111-4111-8111-111111111111";
const LOCAL_HMAC_KEY = "shared-local-development-hmac-key";
const CREDENTIAL_ID = "cG9ydGFibGUtY3JlZGVudGlhbA";
const PUBLIC_KEY = "0x01020304";

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
      ["https://nanocodex.example", "nanocodex.example"],
      ["https://localhost", "localhost"],
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
      "http://branch.nanocodex.localhost:5273/webauthn/login/options",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          origin: "http://nanocodex.localhost:5173",
        },
        body: "{}",
      },
    ), env, new URL("http://branch.nanocodex.localhost:5273/webauthn/login/options"));
    expect(rejected?.status).toBe(403);
  });

  it("carries one signed credential into an isolated local auth store", async () => {
    const source = portableEnv();
    const sessionToken = "s".repeat(43);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });

    const migrated = await routeAccountRequest(new Request("http://one.nanocodex.localhost:5173/v1/me", {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL("http://one.nanocodex.localhost:5173/v1/me"));
    expect(migrated?.status).toBe(200);
    const setCookie = migrated!.headers.get("set-cookie");
    expect(setCookie).toContain("nanocodex_local_passkey=");
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
      "http://two.nanocodex.localhost:5273/webauthn/login/options",
      {
        method: "POST",
        headers: {
          cookie: portableCookie,
          "content-type": "application/json",
          origin: "http://two.nanocodex.localhost:5273",
        },
        body: JSON.stringify({ allowCredentialIds: ["stale-credential"] }),
      },
    ), target.env, new URL("http://two.nanocodex.localhost:5273/webauthn/login/options"));
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

  it("does not import tampered, mismatched, unsigned, or differently signed records", async () => {
    const source = portableEnv();
    const sessionToken = "t".repeat(43);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request("http://one.nanocodex.localhost:5173/v1/me", {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL("http://one.nanocodex.localhost:5173/v1/me"));
    const portableCookie = migrated!.headers.get("set-cookie")!.split(";", 1)[0]!;
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

  it("never issues or imports the portable record on production or generic loopback origins", async () => {
    const source = portableEnv();
    const portableSessionToken = "v".repeat(43);
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
    const portableCookie = migrated!.headers.get("set-cookie")!.split(";", 1)[0]!;

    for (const origin of [
      "https://nanocodex.example",
      "https://localhost",
      "https://127.0.0.1",
      "http://branch.example",
    ]) {
      const local = portableEnv();
      const sessionToken = "u".repeat(43);
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
      expect(response?.headers.get("set-cookie")).toBeNull();

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
  return {
    env: {
      NANOCODEX_AUTH: auth,
      NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: secret,
    } as unknown as AccountAuthEnv,
    get: (name, key) => store(name).get(key),
    set: (name, key, value) => store(name).set(key, value),
  };
}

function loginWithPortableCookie(
  env: AccountAuthEnv,
  cookie: string,
  credentialId: string,
  origin = "http://two.nanocodex.localhost:5273",
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
    persistent,
    createdAt: 1,
    lastAuthenticatedAt: 1,
  };
}
