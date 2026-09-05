import assert from "node:assert/strict";
import { createServer } from "node:http";
import test from "node:test";
import worker, { type Env } from "../src/worker.ts";

const signingSecret = "test-signing-secret-which-is-not-real";
const whatsappAppSecret = "test-whatsapp-app-secret-not-real";
const whatsappVerifyToken = "test-whatsapp-verify-token";
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

test("WhatsApp webhook verification and POST signatures are enforced by the adapter", async () => {
  const configured = withWhatsApp(env());
  const accepted = await worker.fetch(new Request(
    `https://chief.example/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=${whatsappVerifyToken}&hub.challenge=meta-challenge`,
  ), configured);
  assert.equal(accepted.status, 200);
  assert.equal(await accepted.text(), "meta-challenge");

  const rejectedChallenge = await worker.fetch(new Request(
    "https://chief.example/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=wrong&hub.challenge=nope",
  ), configured);
  assert.equal(rejectedChallenge.status, 403);

  const unsigned = await worker.fetch(new Request("https://chief.example/webhooks/whatsapp", {
    body: JSON.stringify({ entry: [] }),
    headers: { "content-type": "application/json" },
    method: "POST",
  }), configured);
  assert.equal(unsigned.status, 401);

  const invalidJson = "{";
  const malformed = await worker.fetch(new Request("https://chief.example/webhooks/whatsapp", {
    body: invalidJson,
    headers: { "x-hub-signature-256": await whatsappSignature(invalidJson) },
    method: "POST",
  }), configured);
  assert.equal(malformed.status, 400);

  const disallowed = await worker.fetch(new Request("https://chief.example/webhooks/whatsapp", {
    method: "PUT",
  }), configured);
  assert.equal(disallowed.status, 405);
});

test("a signed webhook for another WhatsApp phone is fenced before state mutation", async () => {
  const fixture = statefulEnv();
  withWhatsApp(fixture.env);
  const body = JSON.stringify({
    object: "whatsapp_business_account",
    entry: [{
      changes: [{
        field: "messages",
        value: {
          messaging_product: "whatsapp",
          metadata: {
            display_phone_number: "+1 555 999 9999",
            phone_number_id: "999999999999999",
          },
        },
      }],
    }],
  });

  const response = await worker.fetch(new Request("https://chief.example/webhooks/whatsapp", {
    body,
    headers: { "x-hub-signature-256": await whatsappSignature(body) },
    method: "POST",
  }), fixture.env);

  assert.equal(response.status, 403);
  assert.equal(fixture.names.length, 0);
  assert.equal(fixture.values.size, 0);
  assert.equal(fixture.turns.length, 0);
});

test("a signed WhatsApp DM reaches the exact durable route and posts its reply", async () => {
  const fixture = statefulEnv();
  withWhatsApp(fixture.env);
  const graphRequests: {
    authorization: string | null;
    body: Record<string, unknown>;
    method: string | undefined;
    path: string | undefined;
  }[] = [];
  const graph = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += chunk;
    graphRequests.push({
      authorization: request.headers.authorization ?? null,
      body: JSON.parse(body) as Record<string, unknown>,
      method: request.method,
      path: request.url,
    });
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ messages: [{ id: `wamid.reply-${graphRequests.length}` }] }));
  });
  await new Promise<void>((resolve) => graph.listen(0, "127.0.0.1", resolve));
  const address = graph.address();
  if (!address || typeof address === "string") throw new Error("Graph fixture did not bind");
  fixture.env.WHATSAPP_API_URL = `http://127.0.0.1:${address.port}`;
  const body = JSON.stringify({
    object: "whatsapp_business_account",
    entry: [{
      id: "business-account",
      changes: [{
        field: "messages",
        value: {
          contacts: [{ profile: { name: "Alice" }, wa_id: "15551234567" }],
          messages: [{
            from: "15551234567",
            id: "wamid.inbound-1",
            text: { body: "What is on my agenda?" },
            timestamp: "1700000000",
            type: "text",
          }],
          messaging_product: "whatsapp",
          metadata: {
            display_phone_number: "+1 555 000 0000",
            phone_number_id: "123456789012345",
          },
        },
      }],
    }],
  });
  const tasks: Promise<unknown>[] = [];
  try {
    const response = await worker.fetch(new Request("https://chief.example/webhooks/whatsapp", {
      body,
      headers: {
        "content-type": "application/json",
        "x-hub-signature-256": await whatsappSignature(body),
      },
      method: "POST",
    }), fixture.env, {
      waitUntil(task: Promise<unknown>) { tasks.push(task); },
    } as unknown as ExecutionContext);
    assert.equal(response.status, 200);
    await Promise.all(tasks);
    assert.equal(fixture.turns.length, 1);
    assert.equal(fixture.names.includes("chat-sdk:whatsapp"), true);
    assert.equal(fixture.names.some((name) => name.startsWith("conversation:")), true);
    assert.deepEqual(fixture.turns[0], {
      actorId: "15551234567",
      channel: {
        businessPhoneNumberId: "123456789012345",
        conversationId: "whatsapp:123456789012345:15551234567",
        platform: "whatsapp",
        userId: "15551234567",
      },
      messageId: "wamid.inbound-1",
      text: "What is on my agenda?",
    });
    const outbound = graphRequests.find(({ body: requestBody }) =>
      isRecord(requestBody.text) && requestBody.text.body === "Your agenda is clear."
    );
    assert.ok(outbound);
    assert.equal(outbound.authorization, `Bearer ${fixture.env.WHATSAPP_ACCESS_TOKEN}`);
    assert.equal(outbound.method, "POST");
    assert.equal(outbound.path, "/v25.0/123456789012345/messages");
    assert.equal(outbound.body.to, "15551234567");

    const firstGraphRequestCount = graphRequests.length;
    const replayTasks: Promise<unknown>[] = [];
    const replay = await worker.fetch(new Request("https://chief.example/webhooks/whatsapp", {
      body,
      headers: { "x-hub-signature-256": await whatsappSignature(body) },
      method: "POST",
    }), fixture.env, {
      waitUntil(task: Promise<unknown>) { replayTasks.push(task); },
    } as unknown as ExecutionContext);
    await Promise.all(replayTasks);
    assert.equal(replay.status, 200);
    assert.equal(fixture.turns.length, 1);
    assert.equal(graphRequests.length, firstGraphRequestCount);
  } finally {
    await new Promise<void>((resolve, reject) => graph.close((error) => error ? reject(error) : resolve()));
  }
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

test("a second account cannot replace an owned Slack workspace installation", async () => {
  const fixture = statefulEnv();
  fixture.metadata.set("T123ABC", {
    accountId,
    botUserId: "U123ABC",
    installedAt: 1,
    teamId: "T123ABC",
    teamName: "Acme",
  });
  fixture.values.set("slack:installation:T123ABC", "encrypted-original-installation");
  fixture.env.NANOCODEX_BACKEND = {
    async requestingAccountId() { return "00000000-0000-4000-8000-000000000002"; },
    async createAgent() { throw new Error("not used"); },
    async runTurn() { throw new Error("not used"); },
  };
  const start = await worker.fetch(new Request("https://chief.example/v1/slack/install"), fixture.env);
  const state = new URL(start.headers.get("location")!).searchParams.get("state")!;
  const slack = createServer((request, response) => {
    if (request.url === "/api/oauth.v2.access") {
      response.setHeader("content-type", "application/json");
      response.end(JSON.stringify({
        access_token: "xoxb-replacement-secret",
        app_id: "A123ABC",
        bot_user_id: "U999XYZ",
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
      "https://nanocodex.example/demos/chief-of-staff?slack=workspace_already_installed",
    );
    assert.equal(fixture.metadata.get("T123ABC")?.accountId, accountId);
    assert.equal(
      fixture.values.get("slack:installation:T123ABC"),
      "encrypted-original-installation",
    );
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
  const configured = withWhatsApp(env([{
    accountId,
    botUserId: "U123ABC",
    installedAt: 1,
    teamId: "T123ABC",
    teamName: "Acme",
  }]));
  configured.NANOCODEX_BACKEND = {
    async requestingAccountId() { return accountId; },
    async createAgent() { throw new Error("not used"); },
    async runTurn() { throw new Error("not used"); },
  };
  const response = await worker.fetch(new Request("https://chief.example/v1/readiness", {
    headers: { authorization: "Bearer browser-session" },
  }), configured);
  const serialized = await response.text();

  assert.equal(response.status, 200);
  assert.equal(serialized.includes(configured.SLACK_CLIENT_SECRET!), false);
  assert.equal(serialized.includes(configured.SLACK_ENCRYPTION_KEY!), false);
  assert.equal(serialized.includes(configured.SLACK_SIGNING_SECRET!), false);
  assert.equal(serialized.includes(configured.VIBER_AUTH_TOKEN!), false);
  assert.equal(serialized.includes(configured.WHATSAPP_ACCESS_TOKEN!), false);
  assert.equal(serialized.includes(configured.WHATSAPP_APP_SECRET!), false);
  assert.equal(serialized.includes(configured.WHATSAPP_PHONE_NUMBER_ID!), false);
  assert.equal(serialized.includes(configured.WHATSAPP_VERIFY_TOKEN!), false);
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
    { id: "whatsapp", availability: "configured" },
    { id: "imessage", availability: "not_enabled" },
    { id: "viber", availability: "ready" },
  ]);
});

test("signed Viber messages preserve the exact token and route one replay-safe reply", async () => {
  const configured = env();
  const calls: { stateBody?: unknown; stateName?: string; viberBody?: unknown } = {};
  let delivered = false;
  let outboundCalls = 0;
  configured.CHIEF_OF_STAFF_STATE = {
    getByName(name) {
      calls.stateName = name;
      return {
        async fetch(request) {
          const url = new URL(request.url);
          const body = await request.json<Record<string, unknown>>();
          if (url.pathname === "/conversation/turn") {
            calls.stateBody = body;
            return Response.json({ finalMessage: "Your durable reply" });
          }
          if (body.operation === "claim") {
            return Response.json(delivered
              ? { status: "completed" }
              : { status: "claimed", token: "delivery-claim" });
          }
          if (body.operation === "complete") delivered = true;
          return Response.json({ status: body.operation });
        },
      };
    },
  };
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    outboundCalls += 1;
    assert.equal(input, "https://chatapi.viber.com/pa/send_message");
    assert.equal(new Headers(init?.headers).get("x-viber-auth-token"), configured.VIBER_AUTH_TOKEN);
    calls.viberBody = JSON.parse(String(init?.body));
    return Response.json({ message_token: 1, status: 0, status_message: "ok" });
  };
  try {
    const body = JSON.stringify({
      event: "message",
      message_token: 5741311803571721087,
      sender: { id: "01234567890A=", name: "Ada" },
      message: { type: "text", text: "Remember kiwi" },
      timestamp: 1457764197627,
    }).replace("5741311803571721000", "5741311803571721087");
    const signedRequest = async () => new Request("https://chief.example/webhooks/viber", {
      body,
      headers: {
        "content-type": "application/json",
        "x-viber-content-signature": await viberSignature(body, configured.VIBER_AUTH_TOKEN!),
      },
      method: "POST",
    });
    const response = await worker.fetch(await signedRequest(), configured);

    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { status: 0 });
    assert.deepEqual(calls.stateBody, {
      actorId: "01234567890A=",
      channel: {
        botUri: "nanocodex-chief",
        conversationId: "dm:01234567890A=",
        platform: "viber",
        userId: "01234567890A=",
      },
      messageId: "5741311803571721087",
      text: "Remember kiwi",
    });
    assert.match(calls.stateName ?? "", /^conversation:[0-9a-f]{64}$/);
    assert.deepEqual(calls.viberBody, {
      receiver: "01234567890A=",
      min_api_version: 1,
      sender: { name: "Nanocodex" },
      tracking_data: "reply-to:5741311803571721087",
      type: "text",
      text: "Your durable reply",
    });

    const replay = await worker.fetch(await signedRequest(), configured);
    assert.equal(replay.status, 200);
    assert.equal(outboundCalls, 1);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("Viber rejects callbacks before routing when the signature is invalid", async () => {
  const response = await worker.fetch(new Request("https://chief.example/webhooks/viber", {
    body: JSON.stringify({ event: "message" }),
    headers: { "x-viber-content-signature": "0".repeat(64) },
    method: "POST",
  }), env());

  assert.equal(response.status, 401);
  assert.deepEqual(await response.json(), { error: "invalid_signature" });
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
    NANOCODEX_BACKEND: {
      async requestingAccountId() { return accountId; },
      async createAgent() { throw new Error("not used"); },
      async runTurn() { throw new Error("not used"); },
    },
    SLACK_CLIENT_ID: "123.456",
    SLACK_CLIENT_SECRET: "test-client-secret",
    SLACK_ENCRYPTION_KEY: key,
    SLACK_OAUTH_STATE_SECRET: key,
    SLACK_SIGNING_SECRET: signingSecret,
    VIBER_AUTH_TOKEN: "viber-test-token-that-is-long-enough-and-not-real",
    VIBER_BOT_NAME: "Nanocodex",
    VIBER_BOT_URI: "nanocodex-chief",
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
  names: string[];
  turns: unknown[];
} {
  const metadata = new Map();
  const names: string[] = [];
  const values = new Map<string, unknown>();
  const turns: unknown[] = [];
  const configured = env([]);
  configured.CHIEF_OF_STAFF_STATE = {
    getByName(name) {
      names.push(name);
      return {
        async fetch(request) {
          const url = new URL(request.url);
          if (url.pathname === "/slack/installations/claim" && request.method === "POST") {
            const body = await request.json<{ accountId: string; teamId: string }>();
            const ownerKey = `slack:owner:${body.teamId}`;
            const current = values.get(ownerKey)
              ?? (metadata.get(body.teamId) as { accountId?: string } | undefined)?.accountId;
            if (current !== undefined && current !== body.accountId) {
              return Response.json({ error: "workspace_already_installed" }, { status: 409 });
            }
            values.set(ownerKey, body.accountId);
            return new Response(null, { status: 204 });
          }
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
            const ownerKey = `slack:owner:${body.teamId}`;
            const owner = values.get(ownerKey)
              ?? (metadata.get(body.teamId) as { accountId?: string } | undefined)?.accountId;
            if (owner !== undefined && owner !== body.accountId) {
              return Response.json({ error: "account_forbidden" }, { status: 409 });
            }
            if (request.method === "PUT") metadata.set(body.teamId, body);
            if (request.method === "DELETE") {
              metadata.delete(body.teamId);
              values.delete(ownerKey);
            }
            return new Response(null, { status: 204 });
          }
          if (url.pathname === "/chat-sdk") {
            const body = await request.json<{
              key?: string;
              operation: string;
              threadId?: string;
              value?: unknown;
            }>();
            if (body.operation === "get") return Response.json({ value: values.get(body.key!) ?? null });
            if (body.operation === "set") values.set(body.key!, body.value);
            if (body.operation === "delete") values.delete(body.key!);
            if (body.operation === "setIfNotExists") {
              if (values.has(body.key!)) return Response.json({ value: false });
              values.set(body.key!, body.value);
              return Response.json({ value: true });
            }
            if (body.operation === "isSubscribed") {
              return Response.json({ value: values.get(`subscription:${body.threadId}`) === true });
            }
            if (body.operation === "subscribe") {
              values.set(`subscription:${body.threadId}`, true);
            }
            return Response.json({ value: null });
          }
          if (url.pathname === "/conversation/turn" && request.method === "POST") {
            turns.push(await request.json());
            return Response.json({ finalMessage: "Your agenda is clear." });
          }
          return Response.json({ error: "not_found" }, { status: 404 });
        },
      };
    },
  };
  return { env: configured, metadata, names, turns, values };
}

function withWhatsApp(configured: Env): Env {
  configured.WHATSAPP_ACCESS_TOKEN = "test-access-token-which-is-long-enough-for-readiness";
  configured.WHATSAPP_APP_SECRET = whatsappAppSecret;
  configured.WHATSAPP_PHONE_NUMBER_ID = "123456789012345";
  configured.WHATSAPP_VERIFY_TOKEN = whatsappVerifyToken;
  return configured;
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

async function viberSignature(body: string, token: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(token),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const result = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body));
  return Buffer.from(result).toString("hex");
}

async function whatsappSignature(body: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(whatsappAppSecret),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const result = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body));
  return `sha256=${Buffer.from(result).toString("hex")}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
