import assert from "node:assert/strict";
import { test } from "node:test";
import {
  authenticatePrivy,
  routeHostPrincipal,
  type Env,
  type HostClaims,
} from "../worker/host-principal.ts";

const origin = "https://privy.example.test";
const exchangeToken = "e".repeat(43);
const env = {
  PRIVY_APP_ID: "privy-app-123",
  PRIVY_APP_SECRET: "privy-provider-secret",
  NANOCODEX_HOST_APP_ID: "privy-example",
  NANOCODEX_HOST_APP_ORIGIN: origin,
  NANOCODEX_HOST_PROJECT_SECRET: "nanocodex-project-secret-with-32-random-characters",
  NANOCODEX_API_URL: "https://api.nanocodex.xyz",
  NANOCODEX_CONNECT_DIALOG_URL: "https://connect.example.test/dialog/",
} satisfies Env;

function request(method = "POST", options: Readonly<{
  origin?: string;
  cookie?: string;
  body?: unknown;
}> = {}) {
  return new Request(`${origin}/api/nanocodex/host-principal`, {
    method,
    headers: {
      "content-type": "application/json",
      origin: options.origin ?? origin,
      "sec-fetch-site": "same-origin",
      cookie: options.cookie ?? "privy-token=provider-access-token",
    },
    ...(method === "POST" ? {
      body: JSON.stringify(options.body ?? { resources: ["urn:nanocodex:agent:run"] }),
    } : {}),
  });
}

function privy(claims: Readonly<{ user_id?: string; session_id?: string }> = {
  user_id: "did:privy:user-1",
  session_id: "privy-session-1",
}) {
  return {
    utils: () => ({ auth: () => ({
      async verifyAuthToken(token: string) {
        assert.equal(token, "provider-access-token");
        return claims;
      },
    }) }),
  };
}

test("POST exchanges only four server-verified claims and returns an opaque token", async () => {
  let upstream: unknown;
  const response = await routeHostPrincipal(request(), env, {
    createPrivy: () => privy(),
    createIssuer(configuration) {
      return {
        handler({ authenticate }) {
          return async (incoming) => {
            const claims = await authenticate(incoming);
            assert.ok(claims);
            const body = await incoming.json() as { resources: string[] };
            upstream = {
              app_origin: configuration.appOrigin,
              issuer: claims.issuer,
              tenant: claims.tenant,
              subject: claims.subject,
              session_id: claims.sessionId,
              resources: body.resources,
            };
            return Response.json({
              token: exchangeToken,
              expires_at: Math.floor(Date.now() / 1_000) + 60,
            }, { headers: { "cache-control": "no-store" } });
          };
        },
        async revoke() {},
      };
    },
  });

  assert.equal(response?.status, 200);
  assert.deepEqual(upstream, {
    app_origin: origin,
    issuer: "privy",
    tenant: "privy-app-123",
    subject: "did:privy:user-1",
    session_id: "privy-session-1",
    resources: ["urn:nanocodex:agent:run"],
  });
  const output = JSON.stringify(await response?.json());
  assert.match(output, new RegExp(exchangeToken));
  assert.doesNotMatch(output, /provider-access-token|privy-provider-secret|did:privy|privy-session/);
  assert.equal(response?.headers.get("cache-control"), "no-store");
  const cookie = response?.headers.get("set-cookie") ?? "";
  assert.match(cookie, /^__Host-nanocodex-privy-session=[A-Za-z0-9_-]+;/);
  assert.match(cookie, /HttpOnly; Secure; SameSite=Strict/);
  assert.doesNotMatch(cookie.split(";", 1)[0]?.split("=", 2)[1] ?? "", /did:privy|privy-session|provider-access-token/);
});

test("DELETE reauthenticates the still-live cookie before revoking the exact session", async () => {
  let revoked: (HostClaims & Readonly<{ signal?: AbortSignal }>) | undefined;
  const response = await routeHostPrincipal(request("DELETE"), env, {
    createPrivy: () => privy(),
    createIssuer() {
      return {
        handler() { throw new Error("POST handler must not run"); },
        async revoke(claims) { revoked = claims; },
      };
    },
  });
  assert.equal(response?.status, 204);
  const { signal, ...claims } = revoked!;
  assert.equal(signal instanceof AbortSignal, true);
  assert.deepEqual(claims, {
    issuer: "privy",
    tenant: "privy-app-123",
    subject: "did:privy:user-1",
    sessionId: "privy-session-1",
  });
  assert.equal(response?.headers.get("cache-control"), "no-store");
});

test("DELETE revokes the sealed old session after Privy auth disappears", async () => {
  const issued = await routeHostPrincipal(request(), env, {
    createPrivy: () => privy(),
    createIssuer() {
      return {
        handler({ authenticate }) {
          return async (incoming) => {
            assert.ok(await authenticate(incoming));
            return Response.json({ token: exchangeToken, expires_at: 1 });
          };
        },
        async revoke() {},
      };
    },
  });
  const sealedCookie = issued?.headers.get("set-cookie")?.split(";", 1)[0];
  assert.ok(sealedCookie);

  let revoked: HostClaims | undefined;
  const response = await routeHostPrincipal(request("DELETE", { cookie: sealedCookie }), env, {
    createPrivy: () => privy({}),
    createIssuer() {
      return {
        handler() { throw new Error("POST handler must not run"); },
        async revoke(claims) { revoked = claims; },
      };
    },
  });

  assert.equal(response?.status, 204);
  assert.deepEqual(revoked && {
    issuer: revoked.issuer,
    tenant: revoked.tenant,
    subject: revoked.subject,
    sessionId: revoked.sessionId,
  }, {
    issuer: "privy",
    tenant: "privy-app-123",
    subject: "did:privy:user-1",
    sessionId: "privy-session-1",
  });
  assert.match(response?.headers.get("set-cookie") ?? "", /Max-Age=0/);
});

test("a cold-start account switch revokes sealed old claims before replacing them", async () => {
  const first = await routeHostPrincipal(request(), env, {
    createPrivy: () => privy(),
    createIssuer() {
      return {
        handler() { return async () => Response.json({ token: exchangeToken, expires_at: 1 }); },
        async revoke() {},
      };
    },
  });
  const sealedCookie = first?.headers.get("set-cookie")?.split(";", 1)[0];
  assert.ok(sealedCookie);

  const events: string[] = [];
  const switched = await routeHostPrincipal(request("POST", {
    cookie: `${sealedCookie}; privy-token=provider-access-token`,
  }), env, {
    createPrivy: () => privy({ user_id: "did:privy:user-2", session_id: "privy-session-2" }),
    createIssuer() {
      return {
        handler({ authenticate }) {
          return async (incoming) => {
            events.push(`mint:${(await authenticate(incoming))?.subject}`);
            return Response.json({ token: exchangeToken, expires_at: 1 });
          };
        },
        async revoke(claims) { events.push(`revoke:${claims.subject}`); },
      };
    },
  });

  assert.equal(switched?.status, 200);
  assert.deepEqual(events, ["revoke:did:privy:user-1", "mint:did:privy:user-2"]);
  assert.notEqual(switched?.headers.get("set-cookie")?.split(";", 1)[0], sealedCookie);
});

test("cold-start provider loss clears a sealed-cookie slot idempotently", async () => {
  let issuerUsed = false;
  const response = await routeHostPrincipal(request("DELETE", { cookie: "other=value" }), env, {
    createPrivy: () => privy({}),
    createIssuer() {
      issuerUsed = true;
      return { handler() { throw new Error("unused"); }, async revoke() {} };
    },
  });
  assert.equal(response?.status, 204);
  assert.equal(issuerUsed, true);
  assert.match(response?.headers.get("set-cookie") ?? "", /Max-Age=0/);
});

test("origin, method, cookie, and bounded claims fail closed", async () => {
  let dependenciesUsed = false;
  const forbidden = await routeHostPrincipal(request("POST", {
    origin: "https://evil.example",
  }), env, {
    createPrivy() {
      dependenciesUsed = true;
      return privy();
    },
  });
  assert.equal(forbidden?.status, 403);
  assert.equal(dependenciesUsed, false);

  assert.equal((await routeHostPrincipal(request("PUT"), env))?.status, 405);
  assert.equal((await authenticatePrivy(request("DELETE", { cookie: "other=value" }), privy(), "tenant")), undefined);
  assert.equal((await authenticatePrivy(
    request("DELETE"),
    privy({ user_id: "valid", session_id: `bad\nsession` }),
    "tenant",
  )), undefined);
  assert.equal((await authenticatePrivy(
    request("DELETE"),
    privy({ user_id: "x".repeat(513), session_id: "valid" }),
    "tenant",
  )), undefined);
});

test("public config contains routing identifiers and never Worker secrets", async () => {
  const response = await routeHostPrincipal(new Request(`${origin}/api/config`), env);
  assert.equal(response?.status, 200);
  const text = JSON.stringify(await response?.json());
  assert.match(text, /privy-app-123|privy-example|api\.nanocodex/);
  assert.doesNotMatch(text, /privy-provider-secret|nanocodex-project-secret/);
  assert.equal(response?.headers.get("cache-control"), "no-store");
});
