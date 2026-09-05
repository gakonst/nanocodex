import assert from "node:assert/strict";
import { test } from "node:test";

import { cloudflareEgress } from "../cloudflare/egress.mjs";
import { scopeCloudflareEgress } from "../cloudflare/egress-subject.mjs";

class FakeWebSocket {
  accepted = 0;
  binaryType = "blob";
  closed = false;

  accept() { this.accepted += 1; }
  close() { this.closed = true; }
}

test("Cloudflare EGRESS owns one broker endpoint and never accepts provider configuration", () => {
  const binding = { fetch: async () => { throw new Error("must stay cold"); } };
  assert.throws(() => cloudflareEgress(), /requires options/);
  assert.throws(
    () => cloudflareEgress({ binding, authMode: "direct" }),
    /does not accept authMode/,
  );
  assert.throws(
    () => cloudflareEgress({ binding, apiKey: "managed-secret" }),
    /provider credentials belong in the private broker/,
  );
  assert.throws(
    () => cloudflareEgress({ binding, subject: "caller-selected" }),
    /does not accept subject/,
  );
  assert.throws(
    () => cloudflareEgress({ binding: {} }),
    /binding must provide fetch/,
  );
});

test("Cloudflare EGRESS sends one fixed placeholder through the private binding", async () => {
    const calls = [];
    const socket = new FakeWebSocket();
    const binding = {
      async fetch(input, init) {
        calls.push({ input: String(input), init });
        return {
          status: 101,
          headers: new Headers({
            "openai-model": "gpt-5.6-luna",
            "x-codex-turn-state": "next-state",
            "x-reasoning-included": "true",
            "x-request-id": "request-1",
          }),
          webSocket: socket,
        };
      },
    };
    const options = cloudflareEgress({
      binding: scopeCloudflareEgress(binding, "c".repeat(64)),
    });
    assert.equal(Object.isFrozen(options), true);
    assert.equal(options.apiBaseUrl, "https://nanocodex.internal/v1");
    assert.equal(options.websocketUrl, "https://nanocodex.internal/v1/responses");

    const connection = await options.createWebSocket(
      options.websocketUrl,
      "runtime-session-1",
      {
        authorization: "host_managed",
        threadId: "runtime-thread-1",
        turnState: "current-state",
      },
    );
    assert.equal(calls.length, 1);
    assert.equal(calls[0].input, "https://nanocodex.internal/v1/responses");
    assert.equal(calls[0].init.method, "GET");
    const headers = calls[0].init.headers;
    assert.equal(headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
    assert.equal(headers.get("chatgpt-account-id"), null);
    assert.equal(headers.get("openai-beta"), "responses_websockets=2026-02-06");
    assert.equal(headers.get("session-id"), "runtime-session-1");
    assert.equal(headers.get("thread-id"), "runtime-thread-1");
    assert.equal(headers.get("x-client-request-id"), "runtime-thread-1");
    assert.equal(headers.get("x-codex-turn-state"), "current-state");
    assert.equal(headers.get("x-nanocodex-subject"), "c".repeat(64));
    assert.equal(headers.get("upgrade"), "websocket");
    assert.equal(socket.accepted, 1);
    assert.equal(socket.binaryType, "arraybuffer");
    assert.deepEqual(connection, {
      socket,
      status: 101,
      requestId: "request-1",
      serverModel: "gpt-5.6-luna",
      reasoningIncluded: true,
      turnState: "next-state",
    });
});

test("Cloudflare EGRESS denies direct authorization and endpoint changes before fetch", async () => {
  let calls = 0;
  const options = cloudflareEgress({
    binding: { async fetch() { calls += 1; } },
  });
  await assert.rejects(
    options.createWebSocket(
      options.websocketUrl,
      "runtime-session-1",
      { authorization: "bearer", bearerToken: "managed-secret" },
    ),
    /requires Transport\.hostManaged authorization/,
  );
  await assert.rejects(
    options.createWebSocket(
      "wss://example.com/v1/responses",
      "runtime-session-1",
      { authorization: "host_managed" },
    ),
    /denied an unexpected Responses WebSocket endpoint/,
  );
  assert.equal(calls, 0);
});

test("Cloudflare EGRESS exposes bounded broker rejection metadata without reading its body", async () => {
  let cancelled = 0;
  const options = cloudflareEgress({
    binding: {
      async fetch() {
        return {
          status: 503,
          headers: new Headers({ "retry-after": "7" }),
          body: { async cancel() { cancelled += 1; } },
        };
      },
    },
  });
  await assert.rejects(
    options.createWebSocket(
      options.websocketUrl,
      "runtime-session-1",
      { authorization: "preconnect" },
    ),
    (error) => {
      assert.equal(error.status, 503);
      assert.equal(error.body, "credential_broker_rejected");
      assert.equal(error.retryAfter, 7);
      assert.doesNotMatch(error.message, /provider response body/);
      return true;
    },
  );
  assert.equal(cancelled, 1);
});
