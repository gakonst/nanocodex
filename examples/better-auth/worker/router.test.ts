import assert from "node:assert/strict";
import { test } from "node:test";

import {
  authenticateBetterAuth,
  canonicalBetterAuthSecret,
  routeRequest,
  type AuthServer,
  type HostClaims,
  type HostPrincipalIssuer,
  type RouterDependencies,
  type WorkerEnv,
} from "./router.ts";

const origin = "https://host.example.test";
const exchange = "e".repeat(43);
const validSecret = "v1.b8IIdr9T0cmkeHFuEsQS7EPz1yZuf6pmaoBC4hZ9vds";

const baseEnv = {
  ASSETS: { async fetch() { return new Response("asset"); } },
  AUTH_DB: {} as D1Database,
  BETTER_AUTH_SECRET: validSecret,
  BETTER_AUTH_GITHUB_CLIENT_ID: "github-client-id",
  BETTER_AUTH_GITHUB_CLIENT_SECRET: "github-client-secret",
  NANOCODEX_HOST_APP_ID: "better-auth-example",
  NANOCODEX_HOST_APP_ORIGIN: origin,
  NANOCODEX_HOST_PROJECT_SECRET: "host-project-secret-with-at-least-32-characters",
} satisfies WorkerEnv;

function auth(session: Awaited<ReturnType<AuthServer["api"]["getSession"]>> = {
  user: { id: "user-1" },
  session: { token: "session-token-1" },
}): AuthServer {
  return {
    api: { async getSession() { return session; } },
    async handler() { return new Response("auth-handler", { status: 202 }); },
  };
}

function runtime(options: Readonly<{
  auth?: AuthServer;
  onClaims?(claims: HostClaims, method: string): void;
  onAuthRoute?(request: Request): void;
}> = {}): RouterDependencies {
  const authServer = options.auth ?? auth();
  return {
    auth() {
      return {
        ...authServer,
        async handler(request) {
          options.onAuthRoute?.(request);
          return authServer.handler(request);
        },
      };
    },
    principal() {
      const principal: HostPrincipalIssuer = {
        handler({ authenticate }) {
          return async (request) => {
            const claims = await authenticate(request);
            if (!claims) return Response.json({ error: "unauthorized" }, { status: 401 });
            options.onClaims?.(claims, request.method);
            return Response.json({
              token: exchange,
              expires_at: Math.floor(Date.now() / 1_000) + 60,
            });
          };
        },
        async revoke(claims) {
          options.onClaims?.({
            issuer: claims.issuer,
            tenant: claims.tenant,
            subject: claims.subject,
            sessionId: claims.sessionId,
          }, "DELETE");
        },
      };
      return principal;
    },
  };
}

function request(path: string, init: RequestInit = {}): Request {
  const headers = new Headers(init.headers);
  const method = init.method ?? "GET";
  if ((method === "POST" || method === "DELETE") && !headers.has("origin")) {
    headers.set("origin", origin);
    headers.set("sec-fetch-site", "same-origin");
  }
  return new Request(`${origin}${path}`, { ...init, method, headers });
}

test("public config and session status expose no identity or server secret", async () => {
  const [configResponse, sessionResponse] = await Promise.all([
    routeRequest(request("/api/config"), baseEnv, runtime()),
    routeRequest(request("/api/session", { headers: { cookie: "better-auth.session_token=secret" } }), baseEnv, runtime()),
  ]);
  assert.deepEqual(await configResponse.json(), {
    configured: true,
    appId: "better-auth-example",
    appOrigin: origin,
  });
  assert.equal(await sessionResponse.text(), '{"authenticated":true}');
  assert.doesNotMatch(
    `${await routeRequest(request("/api/config"), baseEnv, runtime()).then((value) => value.text())}`,
    /github-client|project-secret|session-token|user-1/,
  );
});

test("Better Auth session maps to four bounded host claims and fails closed", async () => {
  const headers = new Headers({ cookie: "better-auth.session_token=secret" });
  assert.deepEqual(await authenticateBetterAuth(new Request(origin, { headers }), auth(), "github-client-id"), {
    issuer: "better-auth:github",
    tenant: "github-client-id",
    subject: "user-1",
    sessionId: "session-token-1",
  });
  assert.equal(await authenticateBetterAuth(new Request(origin), auth(null), "github-client-id"), undefined);
  assert.equal(await authenticateBetterAuth(new Request(origin), auth({
    user: { id: "bad\nsubject" },
    session: { token: "session" },
  }), "github-client-id"), undefined);
  assert.equal(await authenticateBetterAuth(new Request(origin), {
    api: { async getSession() { throw new Error("database unavailable"); } },
    async handler() { return new Response(); },
  }, "github-client-id"), undefined);
});

test("principal exchange is same-origin, authenticated, and returns only an opaque exchange", async () => {
  const seen: HostClaims[] = [];
  const response = await routeRequest(request("/api/nanocodex/host-principal", {
    method: "POST",
    headers: { "content-type": "application/json", cookie: "better-auth.session_token=secret" },
    body: JSON.stringify({ resources: ["urn:nanocodex:authorization:hosted"] }),
  }), baseEnv, runtime({ onClaims: (claims) => seen.push(claims) }));
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "no-store");
  const body = await response.json() as Record<string, unknown>;
  assert.deepEqual(Object.keys(body).sort(), ["expires_at", "token"]);
  assert.equal(body.token, exchange);
  assert.equal(Number.isSafeInteger(body.expires_at), true);
  assert.deepEqual(seen, [{
    issuer: "better-auth:github",
    tenant: "github-client-id",
    subject: "user-1",
    sessionId: "session-token-1",
  }]);
  assert.doesNotMatch(JSON.stringify(body), /user-1|session-token|github-client|project-secret/);
});

test("principal exchange rejects foreign origins before authentication", async () => {
  let authenticated = false;
  const dependencies = runtime({
    auth: {
      api: { async getSession() { authenticated = true; return null; } },
      async handler() { return new Response(); },
    },
  });
  const response = await routeRequest(request("/api/nanocodex/host-principal", {
    method: "POST",
    headers: { origin: "https://attacker.example", "content-type": "application/json" },
    body: JSON.stringify({ resources: ["urn:nanocodex:authorization:hosted"] }),
  }), baseEnv, dependencies);
  assert.equal(response.status, 403);
  assert.equal(authenticated, false);
});

test("DELETE revokes the exact current session and returns no claims", async () => {
  let revoked: HostClaims | undefined;
  const response = await routeRequest(request("/api/nanocodex/host-principal", {
    method: "DELETE",
    headers: { cookie: "better-auth.session_token=secret" },
  }), baseEnv, runtime({ onClaims(claims, method) {
    if (method === "DELETE") revoked = claims;
  } }));
  assert.equal(response.status, 204);
  assert.equal(await response.text(), "");
  assert.deepEqual(revoked, {
    issuer: "better-auth:github",
    tenant: "github-client-id",
    subject: "user-1",
    sessionId: "session-token-1",
  });

  const unauthorized = await routeRequest(request("/api/nanocodex/host-principal", {
    method: "DELETE",
  }), baseEnv, runtime({ auth: auth(null) }));
  assert.equal(unauthorized.status, 401);
});

test("only GitHub sign-in, GitHub callback, and sign-out reach Better Auth", async () => {
  const seen: string[] = [];
  const dependencies = runtime({ onAuthRoute(incoming) {
    seen.push(`${incoming.method} ${new URL(incoming.url).pathname}`);
  } });
  const allowed = await Promise.all([
    routeRequest(request("/api/auth/sign-in/social", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ provider: "github", callbackURL: `${origin}/` }),
    }), baseEnv, dependencies),
    routeRequest(request("/api/auth/callback/github?code=opaque&state=opaque"), baseEnv, dependencies),
    routeRequest(request("/api/auth/sign-out", { method: "POST" }), baseEnv, dependencies),
  ]);
  assert.deepEqual(allowed.map((response) => response.status), [202, 202, 202]);
  assert.deepEqual(seen.sort(), [
    "GET /api/auth/callback/github",
    "POST /api/auth/sign-in/social",
    "POST /api/auth/sign-out",
  ]);

  seen.length = 0;
  const denied = await Promise.all([
    request("/api/auth/get-session"),
    request("/api/auth/list-sessions"),
    request("/api/auth/get-access-token", { method: "POST" }),
    request("/api/auth/refresh-token", { method: "POST" }),
    request("/api/auth/callback/github", { method: "POST" }),
    request("/api/auth/sign-out"),
    request("/api/auth/sign-in/social", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ provider: "google", callbackURL: `${origin}/` }),
    }),
  ].map((incoming) => routeRequest(incoming, baseEnv, dependencies)));
  assert.deepEqual(denied.map((response) => response.status), [404, 404, 404, 404, 405, 405, 403]);
  assert.deepEqual(seen, []);
});

test("configuration requires a durable database and a canonical random Better Auth secret", async () => {
  const encoded = (bytes: Uint8Array) => `v1.${btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")}`;
  assert.equal(canonicalBetterAuthSecret(validSecret), true);
  for (const value of [
    undefined,
    "short",
    "v1.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    "v1.YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXpBQkNERUY",
    encoded(Uint8Array.from({ length: 32 }, (_, index) => index)),
  ]) assert.equal(canonicalBetterAuthSecret(value), false, value);

  const missingDatabase = await routeRequest(request("/api/config"), {
    ...baseEnv,
    AUTH_DB: undefined as unknown as D1Database,
  }, runtime());
  assert.deepEqual(await missingDatabase.json(), { configured: false });
});
