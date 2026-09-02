import assert from "node:assert/strict";
import { createServer } from "node:http";
import test from "node:test";
import worker, { type Env } from "../src/worker.ts";

const signingSecret = "test-signing-secret-which-is-not-real";
const accountId = "00000000-0000-4000-8000-000000000001";
const key = Buffer.alloc(32, 7).toString("base64url");

test("Slack URL verification requires a valid, fresh signature", async () => {
  const body = JSON.stringify({
    challenge: "challenge-value",
    token: "legacy-token-is-not-trusted",
    type: "url_verification",
  });
  const timestamp = Math.floor(Date.now() / 1_000).toString();

  const unsigned = await worker.fetch(slackRequest(body, timestamp, "v0=bad"), env());
  assert.equal(unsigned.status, 401);

  const staleTimestamp = (Number(timestamp) - 360).toString();
  const stale = await worker.fetch(
    slackRequest(body, staleTimestamp, await signature(staleTimestamp, body)),
    env(),
  );
  assert.equal(stale.status, 401);

  const accepted = await worker.fetch(
    slackRequest(body, timestamp, await signature(timestamp, body)),
    env(),
  );
  assert.equal(accepted.status, 200);
  assert.deepEqual(await accepted.json(), { challenge: "challenge-value" });
});

test("signed events from a different Slack workspace are fenced before routing", async () => {
  const body = JSON.stringify({
    api_app_id: "A123ABC",
    event: {
      channel: "D123ABC",
      channel_type: "im",
      event_ts: "1700000000.000001",
      text: "hello",
      ts: "1700000000.000001",
      type: "message",
      user: "U123ABC",
    },
    event_id: "Ev123ABC",
    event_time: 1700000000,
    team_id: "T999XYZ",
    type: "event_callback",
  });
  const timestamp = Math.floor(Date.now() / 1_000).toString();
  const response = await worker.fetch(
    slackRequest(body, timestamp, await signature(timestamp, body)),
    env(),
  );

  assert.equal(response.status, 403);
  assert.deepEqual(await response.json(), { error: "workspace_forbidden" });
});

test("the authenticated owner gets a one-click Slack bot authorization redirect", async () => {
  const response = await worker.fetch(new Request("https://chief.example/v1/slack/install", {
    headers: {
      cookie: "nanocodex_account=opaque",
      origin: "https://nanocodex.example",
    },
  }), env());

  assert.equal(response.status, 302);
  const location = new URL(response.headers.get("location")!);
  assert.equal(location.origin, "https://slack.com");
  assert.equal(location.pathname, "/oauth/v2/authorize");
  assert.equal(location.searchParams.has("user_scope"), false);
  assert.equal(
    location.searchParams.get("redirect_uri"),
    "https://chief.example/v1/slack/callback",
  );
});

test("the Slack callback stores an encrypted bot installation and returns to the account", async () => {
  const fixture = statefulEnv();
  const start = await worker.fetch(new Request("https://chief.example/v1/slack/install"), fixture.env);
  const state = new URL(start.headers.get("location")!).searchParams.get("state")!;
  const slack = createServer((request, response) => {
    if (request.url === "/api/oauth.v2.access") {
      response.setHeader("content-type", "application/json");
      response.end(JSON.stringify({
        access_token: "xoxb-workspace-secret",
        app_id: "A123ABC",
        bot_user_id: "U123ABC",
        ok: true,
        scope: "chat:write",
        team: { id: "T123ABC", name: "Acme" },
        token_type: "bot",
      }));
      return;
    }
    response.statusCode = 404;
    response.end();
  });
  await new Promise<void>((resolve) => slack.listen(0, "127.0.0.1", resolve));
  const address = slack.address();
  if (!address || typeof address === "string") throw new Error("Slack fixture did not bind");
  fixture.env.SLACK_API_URL = `http://127.0.0.1:${address.port}/api/`;
  try {
    const callback = await worker.fetch(new Request(
      `https://chief.example/v1/slack/callback?code=oauth-code&state=${encodeURIComponent(state)}`,
    ), fixture.env);
    assert.equal(callback.status, 303);
    assert.equal(
      callback.headers.get("location"),
      "https://nanocodex.example/demos/chief-of-staff?slack=installed",
    );
    assert.deepEqual([...fixture.metadata.values()].map(({ accountId: _, ...value }) => value), [{
      botUserId: "U123ABC",
      installedAt: [...fixture.metadata.values()][0]!.installedAt,
      teamId: "T123ABC",
      teamName: "Acme",
    }]);
    assert.equal(JSON.stringify([...fixture.values.values()]).includes("xoxb-workspace-secret"), false);
  } finally {
    await new Promise<void>((resolve, reject) => slack.close((error) => error ? reject(error) : resolve()));
  }
});

test("Slack app_uninstalled removes the bot installation without touching the user connector", async () => {
  const fixture = statefulEnv();
  fixture.metadata.set("T123ABC", {
    accountId,
    botUserId: "U123ABC",
    installedAt: 1,
    teamId: "T123ABC",
    teamName: "Acme",
  });
  fixture.values.set("slack:installation:T123ABC", { botToken: "encrypted-token" });
  const body = JSON.stringify({
    event: { type: "app_uninstalled" },
    event_id: "Ev123ABC",
    event_time: 1700000000,
    team_id: "T123ABC",
    type: "event_callback",
  });
  const timestamp = Math.floor(Date.now() / 1_000).toString();

  const response = await worker.fetch(
    slackRequest(body, timestamp, await signature(timestamp, body)),
    fixture.env,
  );

  assert.equal(response.status, 200);
  assert.equal(fixture.metadata.size, 0);
  assert.equal(fixture.values.has("slack:installation:T123ABC"), false);
});

test("readiness reports bot installations without returning provider credentials", async () => {
  const configured = env([{
    accountId,
    botUserId: "U123ABC",
    installedAt: 1,
    teamId: "T123ABC",
    teamName: "Acme",
  }]);
  configured.NANOCODEX_BACKEND = {
    async fetch() {
      return Response.json({ user: { id: accountId } });
    },
  };
  const response = await worker.fetch(new Request("https://chief.example/v1/readiness", {
    headers: { authorization: "Bearer browser-session" },
  }), configured);
  const serialized = await response.text();

  assert.equal(response.status, 200);
  assert.equal(serialized.includes(configured.SLACK_CLIENT_SECRET!), false);
  assert.equal(serialized.includes(configured.SLACK_ENCRYPTION_KEY!), false);
  assert.equal(serialized.includes(configured.SLACK_SIGNING_SECRET!), false);
  const readiness = JSON.parse(serialized) as {
    channels: { id: string; availability: string }[];
    installations: { teamId: string; teamName: string }[];
  };
  assert.deepEqual(readiness.installations, [{
    botUserId: "U123ABC",
    installedAt: 1,
    teamId: "T123ABC",
    teamName: "Acme",
  }]);
  assert.deepEqual(readiness.channels.map(({ id, availability }) => ({ id, availability })), [
    { id: "slack", availability: "ready" },
    { id: "whatsapp", availability: "not_enabled" },
    { id: "imessage", availability: "not_enabled" },
  ]);
});

function env(installations: readonly unknown[] = [{
  accountId,
  botUserId: "U123ABC",
  installedAt: 1,
  teamId: "T123ABC",
  teamName: "Acme",
}]): Env {
  return {
    CHIEF_OF_STAFF_ACCOUNT_ORIGIN: "https://nanocodex.example",
    CHIEF_OF_STAFF_PUBLIC_ORIGIN: "https://chief.example",
    CHIEF_OF_STAFF_STATE: {
      getByName() {
        return {
          async fetch(request) {
            const url = new URL(request.url);
            if (url.pathname === "/slack/installations" && request.method === "GET") {
              return Response.json({ installations });
            }
            return Response.json({ value: null });
          },
        };
      },
    },
    NANOCODEX_API_KEY: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`,
    NANOCODEX_BACKEND: { async fetch() { return Response.json({ user: { id: accountId } }); } },
    SLACK_CLIENT_ID: "123.456",
    SLACK_CLIENT_SECRET: "test-client-secret",
    SLACK_ENCRYPTION_KEY: key,
    SLACK_OAUTH_STATE_SECRET: key,
    SLACK_SIGNING_SECRET: signingSecret,
  };
}

function statefulEnv(): {
  env: Env;
  metadata: Map<string, {
    accountId: string;
    botUserId: string | null;
    installedAt: number;
    teamId: string;
    teamName: string;
  }>;
  values: Map<string, unknown>;
} {
  const metadata = new Map();
  const values = new Map<string, unknown>();
  const configured = env([]);
  configured.CHIEF_OF_STAFF_STATE = {
    getByName() {
      return {
        async fetch(request) {
          const url = new URL(request.url);
          if (url.pathname === "/slack/installations") {
            if (request.method === "GET") {
              return Response.json({ installations: [...metadata.values()] });
            }
            const body = await request.json<{
              accountId: string;
              botUserId: string | null;
              installedAt: number;
              teamId: string;
              teamName: string;
            }>();
            if (request.method === "PUT") metadata.set(body.teamId, body);
            if (request.method === "DELETE") metadata.delete(body.teamId);
            return new Response(null, { status: 204 });
          }
          if (url.pathname === "/chat-sdk") {
            const body = await request.json<{ key?: string; operation: string; value?: unknown }>();
            if (body.operation === "get") return Response.json({ value: values.get(body.key!) ?? null });
            if (body.operation === "set") values.set(body.key!, body.value);
            if (body.operation === "delete") values.delete(body.key!);
            return Response.json({ value: null });
          }
          return Response.json({ error: "not_found" }, { status: 404 });
        },
      };
    },
  };
  return { env: configured, metadata, values };
}

function slackRequest(body: string, timestamp: string, signed: string): Request {
  return new Request("https://chief.example/webhooks/slack", {
    body,
    headers: {
      "content-type": "application/json",
      "x-slack-request-timestamp": timestamp,
      "x-slack-signature": signed,
    },
    method: "POST",
  });
}

async function signature(timestamp: string, body: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(signingSecret),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const result = await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(`v0:${timestamp}:${body}`),
  );
  return `v0=${Buffer.from(result).toString("hex")}`;
}
