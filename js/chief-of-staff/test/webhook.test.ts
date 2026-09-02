import assert from "node:assert/strict";
import test from "node:test";
import worker, { type Env } from "../src/worker.ts";

const signingSecret = "test-signing-secret-which-is-not-real";

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

test("readiness reports capabilities without returning provider credentials", async () => {
  const configured = env();
  configured.NANOCODEX_BACKEND = {
    async fetch(request) {
      const authorization = request.headers.get("authorization");
      return Response.json({ user: { id: authorization?.startsWith("Bearer ncx_live_")
        ? "account-a"
        : "account-a" } });
    },
  };
  const response = await worker.fetch(new Request("https://chief.example/v1/readiness", {
    headers: { authorization: "Bearer browser-session" },
  }), configured);
  const serialized = await response.text();

  assert.equal(response.status, 200);
  assert.equal(serialized.includes(configured.SLACK_BOT_TOKEN!), false);
  assert.equal(serialized.includes(configured.SLACK_SIGNING_SECRET!), false);
  const readiness = JSON.parse(serialized) as { channels: { id: string; availability: string }[] };
  assert.deepEqual(readiness.channels.map(({ id, availability }) => ({ id, availability })), [
    { id: "slack", availability: "ready" },
    { id: "whatsapp", availability: "not_enabled" },
    { id: "imessage", availability: "not_enabled" },
  ]);
});

function env(): Env {
  return {
    CHIEF_OF_STAFF_PUBLIC_ORIGIN: "https://chief.example",
    CHIEF_OF_STAFF_STATE: {
      getByName() {
        return { async fetch() { return Response.json({ value: null }); } };
      },
    },
    NANOCODEX_API_KEY: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`,
    NANOCODEX_BACKEND: { async fetch() { return Response.json({ user: { id: "account-a" } }); } },
    SLACK_BOT_TOKEN: "xoxb-test-token",
    SLACK_BOT_USER_ID: "U123ABC",
    SLACK_SIGNING_SECRET: signingSecret,
    SLACK_TEAM_ID: "T123ABC",
  };
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
