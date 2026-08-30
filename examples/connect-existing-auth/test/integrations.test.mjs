import assert from "node:assert/strict";
import test from "node:test";

import { Session } from "../../../js/bindings/cloud/server/index.mjs";
import { createConnectClient } from "../browser.mjs";
import { createAuth0SessionRoute } from "../auth0/session.mjs";
import { createBetterAuthSessionRoute } from "../better-auth/session.mjs";
import { createPrivySessionRoute } from "../privy/session.mjs";

const appOrigin = "https://app.example.test";
const opaqueToken = "s".repeat(43);

test("the shared browser integration is importable for every provider", () => {
  assert.equal(typeof createConnectClient, "function");
});

function harness() {
  const exchanges = [];
  const sessions = Session.create({
    appId: "existing-auth-example",
    appOrigin,
    secret: "project-secret-that-is-long-enough-123",
    async fetch(input, init) {
      exchanges.push(new Request(input, init));
      return Response.json({ token: opaqueToken, expires_at: 1_900_000_000 }, { status: 201 });
    },
  });
  const request = new Request(`${appOrigin}/api/nanocodex/session`, {
    method: "POST",
    headers: {
      cookie: "host-session=valid; privy-token=privy-access-token",
      origin: appOrigin,
      "sec-fetch-site": "same-origin",
    },
  });
  return { exchanges, request, sessions };
}

async function exchangedSubject(route, state) {
  const response = await route(state.request);
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), { token: opaqueToken, expires_at: 1_900_000_000 });
  assert.equal(state.exchanges.length, 1);
  const body = await state.exchanges[0].json();
  assert.equal(JSON.stringify(body).includes("privy-access-token"), false);
  return body;
}

test("Auth0 example bridges sub and organization without forwarding its session", async () => {
  const state = harness();
  const route = createAuth0SessionRoute({
    auth0: {
      async getSession() {
        return { user: { sub: "auth0|user-123", org_id: "org_acme" } };
      },
    },
    sessions: state.sessions,
  });
  assert.deepEqual(await exchangedSubject(route, state), {
    app_origin: appOrigin,
    subject: "auth0|user-123",
    organization: "org_acme",
  });
});

test("Better Auth example resolves the incoming host headers", async () => {
  const state = harness();
  const route = createBetterAuthSessionRoute({
    auth: {
      api: {
        async getSession({ headers }) {
          assert.equal(headers.get("cookie")?.includes("host-session=valid"), true);
          return { user: { id: "better-auth-user-123" } };
        },
      },
    },
    sessions: state.sessions,
  });
  assert.deepEqual(await exchangedSubject(route, state), {
    app_origin: appOrigin,
    subject: "better-auth-user-123",
  });
});

test("Privy example verifies its token server-side and forwards only userId", async () => {
  const state = harness();
  const route = createPrivySessionRoute({
    privy: {
      async verifyAuthToken(token) {
        assert.equal(token, "privy-access-token");
        return { userId: "did:privy:user-123" };
      },
    },
    sessions: state.sessions,
  });
  assert.deepEqual(await exchangedSubject(route, state), {
    app_origin: appOrigin,
    subject: "did:privy:user-123",
  });
});

test("all examples fail closed when the host login is absent", async () => {
  const request = new Request(`${appOrigin}/api/nanocodex/session`, {
    method: "POST",
    headers: { origin: appOrigin, "sec-fetch-site": "same-origin" },
  });
  const routes = [
    createAuth0SessionRoute({ auth0: { async getSession() {} }, sessions: harness().sessions }),
    createBetterAuthSessionRoute({ auth: { api: { async getSession() {} } }, sessions: harness().sessions }),
    createPrivySessionRoute({ privy: { async verifyAuthToken() { throw new Error("must not run"); } }, sessions: harness().sessions }),
  ];
  for (const route of routes) assert.equal((await route(request.clone())).status, 401);
});
