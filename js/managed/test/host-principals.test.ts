import { afterEach, describe, expect, it, vi } from "vitest";

import {
  parseProjects,
  routeHostPrincipalRequest,
  type HostPrincipal,
  type HostPrincipalEnv,
} from "../src/host-principals";

const INTERNAL = "https://nanocodex.internal";
const APP_ID = "acme";
const APP_ORIGIN = "https://app.acme.test";
const ISSUER = "better-auth";
const OTHER_ISSUER = "privy";
const SECRET = "host-project-secret-that-is-long-enough";
const TENANT = "acme-production";
const OTHER_TENANT = "acme-sandbox";
const RESOURCES = ["urn:nanocodex:agent:run", "urn:nanocodex:connectors:github"];

afterEach(() => vi.useRealTimers());

describe("host principal project policy", () => {
  it("accepts only bounded exact app, origin, issuer, and tenant registrations", async () => {
    const digest = await sha256(SECRET);
    expect(parseProjects(JSON.stringify([{
      app_id: APP_ID,
      app_origin: APP_ORIGIN,
      issuer: ISSUER,
      secret_sha256: digest,
      tenant: TENANT,
    }]))).toEqual([{
      appId: APP_ID,
      appOrigin: APP_ORIGIN,
      issuer: ISSUER,
      secretDigest: digest,
      tenant: TENANT,
    }]);
    expect(() => parseProjects(JSON.stringify([{
      app_id: APP_ID,
      app_origin: "http://app.acme.test",
      issuer: ISSUER,
      secret_sha256: digest,
      tenant: TENANT,
    }]))).toThrow();
    expect(() => parseProjects(JSON.stringify([{
      app_id: APP_ID,
      app_origin: APP_ORIGIN,
      issuer: `${ISSUER}\u0000suffix`,
      secret_sha256: digest,
      tenant: TENANT,
    }]))).toThrow();
    expect(() => parseProjects(JSON.stringify([{
      app_id: APP_ID,
      app_origin: APP_ORIGIN,
      secret_sha256: digest,
      tenant: TENANT,
    }]))).toThrow();
    expect(() => parseProjects(JSON.stringify([
      {
        app_id: APP_ID,
        app_origin: APP_ORIGIN,
        issuer: ISSUER,
        secret_sha256: digest,
        tenant: TENANT,
      },
      {
        app_id: APP_ID,
        app_origin: APP_ORIGIN,
        issuer: ISSUER,
        secret_sha256: digest,
        tenant: TENANT,
      },
    ]))).toThrow();
    expect(parseProjects(JSON.stringify([
      {
        app_id: APP_ID,
        app_origin: APP_ORIGIN,
        issuer: ISSUER,
        secret_sha256: digest,
        tenant: TENANT,
      },
      {
        app_id: APP_ID,
        app_origin: APP_ORIGIN,
        issuer: ISSUER,
        secret_sha256: digest,
        tenant: OTHER_TENANT,
      },
    ]))).toHaveLength(2);
  });

  it("rejects project, origin, resource-authority, and browser mint widening", async () => {
    const fixture = await hostFixture();
    expect((await fixture.mint({ secret: `${SECRET}x` })).status).toBe(401);
    expect((await fixture.mint({ appOrigin: "https://other.acme.test" })).status).toBe(401);
    expect((await fixture.mint({ issuer: "unregistered-issuer" })).status).toBe(401);
    expect((await fixture.mint({ tenant: "unregistered-tenant" })).status).toBe(401);
    expect((await fixture.mint({ issuer: `${ISSUER}\u0000suffix` })).status).toBe(400);
    expect((await fixture.mint({ tenant: "x".repeat(513) })).status).toBe(400);
    expect((await fixture.mint({ origin: APP_ORIGIN })).status).toBe(403);
    for (const resource of [
      "urn:nanocodex:mpp:machusd:spend",
      "urn:nanocodex:access-key:create",
      "urn:nanocodex:authorize-access-key",
    ]) {
      expect((await fixture.mint({ resources: [resource] })).status).toBe(400);
    }
  });
});

describe("host principal exchange protocol", () => {
  it("returns an opaque one-time app/origin/resource-bound exchange and no raw host identity", async () => {
    const fixture = await hostFixture();
    const minted = await fixture.mint();
    expect(minted.status).toBe(201);
    const issued = await minted.json<{ token: string; expires_at: number }>();
    expect(issued.token).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(JSON.stringify(fixture.values())).not.toContain("host-user-123");
    expect(JSON.stringify(fixture.values())).not.toContain("host-session-a");

    const consumed = await fixture.consume(issued.token);
    expect(consumed.status).toBe(200);
    const binding = await consumed.json<{
      principal: HostPrincipal;
      user_id: string;
      resources: string[];
      expires_at: number;
    }>();
    expect(binding).toMatchObject({
      principal: {
        kind: "host",
        app_id: APP_ID,
        app_origin: APP_ORIGIN,
        issuer: ISSUER,
        tenant: TENANT,
        session_epoch: 1,
      },
      resources: RESOURCES,
      expires_at: issued.expires_at,
    });
    expect(binding.principal.id).toBe(await hmacSha256(
      SECRET,
      `host-principal:v2\0${APP_ID}\0${APP_ORIGIN}\0${ISSUER}\0${TENANT}\0host-user-123`,
    ));
    expect(binding.principal.id).not.toBe(await sha256(
      `host-principal:v1\0${APP_ID}\0${APP_ORIGIN}\0host-user-123`,
    ));
    expect(binding.user_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(fixture.provisioned).toContain(binding.user_id);
    expect((await fixture.consume(issued.token)).status).toBe(403);

    const mismatch = await (await fixture.mint()).json<{ token: string }>();
    expect((await fixture.consume(mismatch.token, { appOrigin: "https://other.acme.test" })).status).toBe(403);
    expect((await fixture.consume(mismatch.token)).status).toBe(403);
  });

  it("uses the authenticated project secret to hide a low-entropy subject", async () => {
    const fixture = await hostFixture();
    const subject = "1";
    const binding = await consumeMint(fixture, { subject });
    const namespace = `host-principal:v2\0${APP_ID}\0${APP_ORIGIN}\0${ISSUER}\0${TENANT}\0${subject}`;

    expect(binding.principal.id).toBe(await hmacSha256(SECRET, namespace));
    expect(binding.principal.id).not.toBe(await sha256(namespace));
  });

  it("rejects widened or namespace-mutated private principals", async () => {
    const fixture = await hostFixture();
    const { principal } = await consumeMint(fixture, {});

    expect((await fixture.validate({ ...principal, tenant: OTHER_TENANT })).status).toBe(403);
    expect((await fixture.validate({ ...principal, issuer: "" })).status).toBe(400);
    expect((await fixture.validate({ ...principal, extra: true } as unknown as HostPrincipal)).status).toBe(400);
  });

  it("expires exchanges and atomically fences old epochs on session replacement and revoke", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-02T00:00:00Z"));
    const fixture = await hostFixture();

    const expiring = await (await fixture.mint({ expiresIn: 30 })).json<{ token: string }>();
    vi.advanceTimersByTime(31_000);
    expect((await fixture.consume(expiring.token)).status).toBe(403);

    const first = await (await fixture.mint()).json<{ token: string }>();
    const firstBinding = await (await fixture.consume(first.token)).json<{
      principal: HostPrincipal;
      user_id: string;
    }>();
    const second = await (await fixture.mint({ sessionId: "host-session-b" })).json<{ token: string }>();
    const secondBinding = await (await fixture.consume(second.token)).json<{
      principal: HostPrincipal;
      user_id: string;
    }>();
    expect(secondBinding.user_id).toBe(firstBinding.user_id);
    expect(secondBinding.principal.session_epoch).toBe(firstBinding.principal.session_epoch + 1);
    expect((await fixture.validate(firstBinding.principal)).status).toBe(403);
    expect((await fixture.validate(secondBinding.principal)).status).toBe(200);

    const pending = await (await fixture.mint({ sessionId: "host-session-b" })).json<{ token: string }>();
    expect((await fixture.revoke({ sessionId: "host-session-b" })).status).toBe(204);
    expect((await fixture.validate(secondBinding.principal)).status).toBe(403);
    expect((await fixture.consume(pending.token)).status).toBe(403);
  });

  it("keeps a revoked session fenced when an expired lock holder writes stale records", async () => {
    const fixture = await hostFixture();
    const binding = await consumeMint(fixture, {});
    const mappingKey = `principal:${binding.principal.id}`;
    const sessionKey = `session:${binding.principal.session_digest}`;
    const staleMapping = fixture.read(mappingKey);
    const staleSession = fixture.read(sessionKey);

    expect((await fixture.revoke()).status).toBe(204);
    fixture.write(mappingKey, staleMapping);
    fixture.write(sessionKey, staleSession);

    expect((await fixture.validate(binding.principal)).status).toBe(403);
  });

  it("fences account switches that reuse one host session identifier", async () => {
    const fixture = await hostFixture();
    const first = await (await fixture.mint()).json<{ token: string }>();
    const firstBinding = await (await fixture.consume(first.token)).json<{
      principal: HostPrincipal;
      user_id: string;
    }>();
    const switched = await (await fixture.mint({ subject: "host-user-456" })).json<{ token: string }>();
    const switchedBinding = await (await fixture.consume(switched.token)).json<{
      principal: HostPrincipal;
      user_id: string;
    }>();
    expect(switchedBinding.principal.id).not.toBe(firstBinding.principal.id);
    expect(switchedBinding.user_id).not.toBe(firstBinding.user_id);
    expect((await fixture.validate(firstBinding.principal)).status).toBe(403);
    expect((await fixture.validate(switchedBinding.principal)).status).toBe(200);

    const switchedBack = await (await fixture.mint()).json<{ token: string }>();
    const rebound = await (await fixture.consume(switchedBack.token)).json<{
      principal: HostPrincipal;
      user_id: string;
    }>();
    expect(rebound.principal.id).toBe(firstBinding.principal.id);
    expect(rebound.user_id).toBe(firstBinding.user_id);
    expect(rebound.principal.session_epoch).toBeGreaterThan(switchedBinding.principal.session_epoch);
    expect((await fixture.validate(firstBinding.principal)).status).toBe(403);
    expect((await fixture.validate(switchedBinding.principal)).status).toBe(403);
    expect((await fixture.validate(rebound.principal)).status).toBe(200);
  });

  it("isolates the same subject and session across tenants and issuers", async () => {
    const fixture = await hostFixture();
    const primary = await consumeMint(fixture, {});
    const otherTenant = await consumeMint(fixture, { tenant: OTHER_TENANT });
    const otherIssuer = await consumeMint(fixture, { issuer: OTHER_ISSUER });

    expect(new Set([
      primary.principal.id,
      otherTenant.principal.id,
      otherIssuer.principal.id,
    ])).toHaveProperty("size", 3);
    expect(new Set([
      primary.principal.session_digest,
      otherTenant.principal.session_digest,
      otherIssuer.principal.session_digest,
    ])).toHaveProperty("size", 3);
    expect(new Set([
      primary.user_id,
      otherTenant.user_id,
      otherIssuer.user_id,
    ])).toHaveProperty("size", 3);

    expect((await fixture.revoke()).status).toBe(204);
    expect((await fixture.validate(primary.principal)).status).toBe(403);
    expect((await fixture.validate(otherTenant.principal)).status).toBe(200);
    expect((await fixture.validate(otherIssuer.principal)).status).toBe(200);

    expect((await fixture.revoke({ tenant: OTHER_TENANT })).status).toBe(204);
    expect((await fixture.validate(otherTenant.principal)).status).toBe(403);
    expect((await fixture.validate(otherIssuer.principal)).status).toBe(200);
  });
});

async function consumeMint(
  fixture: Awaited<ReturnType<typeof hostFixture>>,
  options: { issuer?: string; subject?: string; tenant?: string },
): Promise<{ principal: HostPrincipal; user_id: string }> {
  const { token } = await (await fixture.mint(options)).json<{ token: string }>();
  return (await fixture.consume(token)).json<{ principal: HostPrincipal; user_id: string }>();
}

async function hostFixture(): Promise<{
  consume(token: string, options?: { appOrigin?: string; resources?: string[] }): Promise<Response>;
  env: HostPrincipalEnv;
  mint(options?: {
    appOrigin?: string;
    expiresIn?: number;
    issuer?: string;
    origin?: string;
    resources?: string[];
    secret?: string;
    sessionId?: string;
    subject?: string;
    tenant?: string;
  }): Promise<Response>;
  provisioned: string[];
  read(key: string): unknown;
  revoke(options?: {
    issuer?: string;
    sessionId?: string;
    subject?: string;
    tenant?: string;
  }): Promise<Response>;
  validate(principal: HostPrincipal): Promise<Response>;
  values(): unknown[];
  write(key: string, value: unknown): void;
}> {
  type Entry = { value: unknown; expiresAt?: number };
  const stores = new Map<string, Map<string, Entry>>();
  const current = (name: string) => {
    let store = stores.get(name);
    if (!store) { store = new Map(); stores.set(name, store); }
    return store;
  };
  const auth = {
    idFromName(name: string) { return name; },
    get(name: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          const url = new URL(request.url);
          const key = url.searchParams.get("key")!;
          const store = current(name);
          const entry = store.get(key);
          const active = entry && (entry.expiresAt === undefined || entry.expiresAt > Date.now());
          if (entry && !active) store.delete(key);
          if (url.pathname === "/get") return Response.json({ value: active ? entry.value : undefined });
          if (url.pathname === "/set") {
            const body = await request.json<{ value: unknown; ttl?: number }>();
            store.set(key, { value: body.value, expiresAt: body.ttl ? Date.now() + body.ttl * 1_000 : undefined });
            return Response.json({ ok: true });
          }
          if (url.pathname === "/create") {
            const body = await request.json<{ value: unknown; ttl?: number }>();
            const created = !active;
            if (created) store.set(key, { value: body.value, expiresAt: body.ttl ? Date.now() + body.ttl * 1_000 : undefined });
            return Response.json({ created });
          }
          if (url.pathname === "/take") {
            if (active) store.delete(key);
            return Response.json({ value: active ? entry.value : undefined });
          }
          if (url.pathname === "/delete") {
            store.delete(key);
            return Response.json({ ok: true });
          }
          return new Response(null, { status: 404 });
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const provisioned: string[] = [];
  const users = {
    getByName(userId: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          if (new URL(request.url).pathname !== "/account" || request.method !== "PUT") {
            return new Response(null, { status: 404 });
          }
          provisioned.push(userId);
          return Response.json({ id: userId });
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const env = {
    NANOCODEX_AUTH: auth,
    NANOCODEX_HOST_PROJECTS: JSON.stringify([{
      app_id: APP_ID,
      app_origin: APP_ORIGIN,
      issuer: ISSUER,
      secret_sha256: await sha256(SECRET),
      tenant: TENANT,
    }, {
      app_id: APP_ID,
      app_origin: APP_ORIGIN,
      issuer: ISSUER,
      secret_sha256: await sha256(SECRET),
      tenant: OTHER_TENANT,
    }, {
      app_id: APP_ID,
      app_origin: APP_ORIGIN,
      issuer: OTHER_ISSUER,
      secret_sha256: await sha256(SECRET),
      tenant: TENANT,
    }]),
    NANOCODEX_USERS: users,
  } as unknown as HostPrincipalEnv;
  const route = (path: string, method: string, body: unknown, headers: HeadersInit = {}) => {
    const request = new Request(`${INTERNAL}${path}`, {
      method,
      headers: { "content-type": "application/json", ...headers },
      body: JSON.stringify(body),
    });
    return routeHostPrincipalRequest(request, env, new URL(request.url)) as Promise<Response>;
  };
  return {
    env,
    provisioned,
    read: (key) => current("host-principals").get(key)?.value,
    values: () => [...stores.values()].flatMap((store) => [...store.values()].map(({ value }) => value)),
    write: (key, value) => current("host-principals").set(key, { value }),
    mint(options = {}) {
      return route("/connect/host-principals/exchanges", "POST", {
        app_origin: options.appOrigin ?? APP_ORIGIN,
        issuer: options.issuer ?? ISSUER,
        subject: options.subject ?? "host-user-123",
        session_id: options.sessionId ?? "host-session-a",
        tenant: options.tenant ?? TENANT,
        resources: options.resources ?? RESOURCES,
        ...(options.expiresIn === undefined ? {} : { expires_in: options.expiresIn }),
      }, {
        authorization: `Bearer ${options.secret ?? SECRET}`,
        "x-nanocodex-app-id": APP_ID,
        ...(options.origin ? { origin: options.origin } : {}),
      });
    },
    consume(token, options = {}) {
      return route("/connect/host-principals/exchange", "POST", {
        exchange: token,
        app_id: APP_ID,
        app_origin: options.appOrigin ?? APP_ORIGIN,
        resources: options.resources ?? RESOURCES,
      });
    },
    revoke(options = {}) {
      return route("/connect/host-principals/sessions", "DELETE", {
        app_origin: APP_ORIGIN,
        issuer: options.issuer ?? ISSUER,
        subject: options.subject ?? "host-user-123",
        session_id: options.sessionId ?? "host-session-a",
        tenant: options.tenant ?? TENANT,
      }, { authorization: `Bearer ${SECRET}`, "x-nanocodex-app-id": APP_ID });
    },
    validate(principal) {
      return route("/connect/host-principals/validate", "POST", { principal });
    },
  };
}

async function sha256(value: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function hmacSha256(secret: string, value: string): Promise<string> {
  const encoder = new TextEncoder();
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const bytes = new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(value)));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
