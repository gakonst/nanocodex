import assert from "node:assert/strict";
import { test } from "node:test";

import { modelConnection, upstreamHeaders, validateWebSocketRequest } from "./protocol.mjs";

test("the Worker adds the secret and stable session headers upstream", () => {
  const headers = upstreamHeaders({ kind: "api_key", apiKey: "worker-secret" }, "browser-session");
  assert.equal(headers.Authorization, "Bearer worker-secret");
  assert.equal(headers.Upgrade, "websocket");
  assert.equal(headers["OpenAI-Beta"], "responses_websockets=2026-02-06");
  assert.equal(headers["x-openai-internal-codex-responses-lite"], "true");
  assert.equal(headers["session-id"], "browser-session");
  assert.equal(headers["thread-id"], "browser-session");
});

test("local Vite auth keeps ChatGPT credentials behind the Worker", () => {
  const expiresAt = Date.now() + 60 * 60_000;
  const connection = modelConnection({
    ENVIRONMENT: "development",
    NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN: "local-access",
    NANOCODEX_DEV_CHATGPT_ACCOUNT_ID: "account-123",
    NANOCODEX_DEV_CHATGPT_FEDRAMP: "false",
    NANOCODEX_DEV_CHATGPT_EXPIRES_AT: String(expiresAt),
    NANOCODEX_DEV_CHATGPT_EGRESS_URL: `http://127.0.0.1:43123/${"e".repeat(43)}/`,
    NANOCODEX_DEV_CHATGPT_SESSION_ID: "s".repeat(43),
  });
  assert.equal(connection.url,
    `http://127.0.0.1:43123/${"e".repeat(43)}/backend-api/codex/responses`);
  const headers = upstreamHeaders(connection.credential, "browser-session");
  assert.equal(headers.Authorization, "Bearer local-access");
  assert.equal(headers["ChatGPT-Account-ID"], "account-123");
  assert.equal(Object.hasOwn(headers, "X-OpenAI-Fedramp"), false);
});

test("production ignores local Vite credential bindings", () => {
  const connection = modelConnection({
    ENVIRONMENT: "production",
    OPENAI_API_KEY: "production-key",
    NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN: "must-be-ignored",
    NANOCODEX_DEV_CHATGPT_ACCOUNT_ID: "must-be-ignored",
    NANOCODEX_DEV_CHATGPT_FEDRAMP: "false",
    NANOCODEX_DEV_CHATGPT_EXPIRES_AT: String(Date.now() + 60 * 60_000),
    NANOCODEX_DEV_CHATGPT_EGRESS_URL: `http://127.0.0.1:43123/${"e".repeat(43)}/`,
    NANOCODEX_DEV_CHATGPT_SESSION_ID: "s".repeat(43),
  });
  assert.equal(connection.credential.kind, "api_key");
  assert.equal(connection.credential.apiKey, "production-key");
});

test("the Worker accepts only same-origin WebSocket upgrades with valid sessions", () => {
  const accepted = new Request("https://app.example/api/responses?session_id=session-1", {
    headers: { Origin: "https://app.example", Upgrade: "websocket" },
  });
  assert.equal(validateWebSocketRequest(accepted), undefined);

  const crossOrigin = new Request("https://app.example/api/responses?session_id=session-1", {
    headers: { Origin: "https://other.example", Upgrade: "websocket" },
  });
  assert.equal(validateWebSocketRequest(crossOrigin)?.status, 403);

  const invalidSession = new Request("https://app.example/api/responses?session_id=bad%20session", {
    headers: { Origin: "https://app.example", Upgrade: "websocket" },
  });
  assert.equal(validateWebSocketRequest(invalidSession)?.status, 400);
});
