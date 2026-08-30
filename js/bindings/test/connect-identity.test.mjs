import assert from "node:assert/strict";
import { test } from "node:test";

import { Actions, Client, Dialog, Identity, Transport } from "../cloud/index.mjs";
import { Session } from "../cloud/server/index.mjs";

const token = "s".repeat(43);

test("host identity reads only an opaque Nanocodex session from a same-origin route", async () => {
  const requests = [];
  const expiresAt = Math.floor(Date.now() / 1_000) + 120;
  const identity = Identity.host({
    async fetch(input, init) {
      requests.push(new Request(input, init));
      return Response.json({
        token,
        expires_at: expiresAt,
      });
    },
  }).setup({ appId: "acme", appOrigin: "https://app.acme.test" });

  assert.deepEqual(await identity.getSession(), {
    token,
    expiresAt,
  });
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, "https://app.acme.test/api/nanocodex/session");
  assert.equal(requests[0].method, "POST");
  assert.equal(requests[0].credentials, "include");
  assert.equal(requests[0].headers.get("x-nanocodex-app-id"), "acme");
  assert.equal(requests[0].headers.has("authorization"), false);
  assert.equal(await requests[0].text(), "");
});

test("host identity cannot send the application session to another origin", () => {
  assert.throws(() => Identity.host({
    url: "https://attacker.test/session",
    fetch() { throw new Error("must not fetch"); },
  }).setup({ appId: "acme", appOrigin: "https://app.acme.test" }), /same|origin/);
});

test("server helper exchanges a verified host subject without exposing the project secret", async () => {
  const requests = [];
  const sessions = Session.create({
    appId: "acme",
    appOrigin: "https://app.acme.test",
    baseUrl: "https://connect.example",
    secret: "project-secret-that-is-long-enough-123",
    async fetch(input, init) {
      requests.push(new Request(input, init));
      return Response.json({
        token,
        expires_at: 1_900_000_000,
      }, { status: 201 });
    },
  });

  assert.deepEqual(await sessions.create({
    subject: "auth0|user-123",
    organization: "org_456",
    expiresIn: 90,
  }), { token, expires_at: 1_900_000_000 });
  assert.equal(requests[0].url, "https://connect.example/v1/embed/sessions");
  assert.equal(requests[0].headers.get("authorization"), "Bearer project-secret-that-is-long-enough-123");
  assert.equal(requests[0].headers.get("x-nanocodex-app-id"), "acme");
  assert.equal(requests[0].headers.has("origin"), false);
  assert.deepEqual(await requests[0].json(), {
    app_origin: "https://app.acme.test",
    subject: "auth0|user-123",
    organization: "org_456",
    expires_in: 90,
  });
});

test("server handler verifies the host login behind an exact-origin POST", async () => {
  let authenticated = 0;
  const sessions = Session.create({
    appId: "acme",
    appOrigin: "https://app.acme.test",
    secret: "project-secret-that-is-long-enough-123",
    async fetch() {
      return Response.json({ token, expires_at: 1_900_000_000 }, { status: 201 });
    },
  });
  const handler = sessions.handler({
    authenticate(request) {
      authenticated += 1;
      assert.equal(request.headers.get("cookie"), "host-session=valid");
      return { subject: "existing-user-123" };
    },
  });

  const rejected = await handler(new Request("https://app.acme.test/api/nanocodex/session", {
    method: "POST",
    headers: { origin: "https://attacker.test", cookie: "host-session=valid" },
  }));
  assert.equal(rejected.status, 403);
  assert.equal(authenticated, 0);

  const accepted = await handler(new Request("https://app.acme.test/api/nanocodex/session", {
    method: "POST",
    headers: {
      origin: "https://app.acme.test",
      "sec-fetch-site": "same-origin",
      cookie: "host-session=valid",
    },
  }));
  assert.equal(accepted.status, 200);
  assert.equal(accepted.headers.get("cache-control"), "no-store");
  assert.deepEqual(await accepted.json(), { token, expires_at: 1_900_000_000 });
  assert.equal(authenticated, 1);
});

test("Connect signs the opaque host session and replaces injected identity resources", async () => {
  const captured = [];
  const expiry = Math.floor(Date.now() / 1_000) + 120;
  const client = Client.create({
    appId: "acme",
    appOrigin: "https://app.acme.test",
    dialog: Dialog.memory(),
    identity: Identity.custom({
      async getSession() {
        return { token, expires_at: expiry };
      },
    }),
    provider: {
      async request(request) {
        captured.push(request);
        throw new Error("stop after signed request capture");
      },
    },
    transport: Transport.mock({ appName: "Acme" }),
  });

  await assert.rejects(Actions.connection.connect(client, {
    capabilities: {
      auth: {
        resources: [
          `urn:nanocodex:identity-session:${"x".repeat(43)}`,
          "documents",
        ],
      },
      cloudAccounts: { github: true },
    },
  }), /signed request capture/);

  const resources = captured[0].params[0].capabilities.auth.resources;
  assert.ok(resources.includes(`urn:nanocodex:identity-session:${token}`));
  assert.equal(resources.some((resource) => resource.includes("x".repeat(43))), false);
  assert.ok(resources.includes("urn:nanocodex:connectors:github"));
  assert.ok(resources.includes("urn:nanocodex:app:acme"));
  assert.ok(resources.includes("urn:nanocodex:origin:https%3A%2F%2Fapp.acme.test"));
});
