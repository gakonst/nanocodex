import assert from "node:assert/strict";
import { test } from "node:test";
import { historyNotesHost, sameOriginHistoryNotes } from "../runtime/history-notes.mjs";
import { cloudflareEgress } from "../cloudflare/egress.mjs";
import { scopeCloudflareEgress } from "../cloudflare/egress-subject.mjs";

const baseUrl = "https://chatgpt.com/backend-api/codex";
const body = { path: "progress", content: "saved", context: { session_id: "session", current_agent_name: "/root" } };
const request = { baseUrl, path: "alpha/notes/v2/write_file", body, budget: { mode: "tokens", limit: 10_000 } };

test("history/notes sends only pinned authenticated endpoints and preserves encrypted media", async () => {
  const calls = [];
  const host = historyNotesHost({ direct: true, fetch: async (url, init) => {
    calls.push({ url, init });
    return Response.json({ encrypted_output: "opaque", images: [{ data: "abc", mime_type: "image/png", detail: "original" }] });
  } });
  assert.equal(await host.capability("thread", baseUrl), "direct");
  assert.equal(await host.capability("thread", "https://provider.example"), "none");
  const result = JSON.parse(await host.request("thread", JSON.stringify(request), "test-token", "account", true));
  assert.equal(result.body.encrypted_output, "opaque");
  assert.equal(result.body.images[0].detail, "original");
  assert.equal(calls[0].url, `${baseUrl}/${request.path}`);
  assert.equal(calls[0].init.headers.get("authorization"), "Bearer test-token");
  assert.equal(calls[0].init.headers.get("chatgpt-account-id"), "account");
  assert.equal(calls[0].init.headers.get("x-openai-fedramp"), "true");
  assert.equal(calls[0].init.headers.get("x-openai-encrypted-tool-arguments"), "true");
  assert.equal(calls[0].init.redirect, "error");
  assert.deepEqual(JSON.parse(calls[0].init.body), body);
  for (const changed of [{ baseUrl: "https://evil.example" }, { path: "alpha/notes/v2/../delete" }, { path: "alpha/notes/v2/delete_all" }]) {
    await assert.rejects(host.request("thread", JSON.stringify({ ...request, ...changed }), "test-token"));
  }
  assert.equal(calls.length, 1);
});

test("private Cloudflare history/notes uses subject ownership and a fixed placeholder", async () => {
  const calls = [];
  const transport = cloudflareEgress({ binding: scopeCloudflareEgress({
    async fetch(input, init) {
      calls.push({ url: String(input), init });
      return Response.json(init.method === "GET" ? { enabled: true } : { encrypted_output: "broker-opaque" });
    },
  }, "B".repeat(64)) });
  const host = historyNotesHost({ broker: transport.historyNotes, apiBaseUrl: transport.apiBaseUrl });
  assert.equal(await host.capability("thread", transport.apiBaseUrl), "host_managed");
  const encoded = JSON.stringify({ ...request, baseUrl: transport.apiBaseUrl });
  const output = JSON.parse(await host.request("thread", encoded, "host-managed"));
  assert.equal(output.body.encrypted_output, "broker-opaque");
  assert.equal(calls[0].url, "https://nanocodex.internal/.well-known/nanocodex/context-management");
  assert.equal(calls[1].url, `${transport.apiBaseUrl}/${request.path}`);
  assert.equal(calls[1].init.headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
  assert.equal(calls[1].init.headers.get("x-nanocodex-subject"), "B".repeat(64));
  assert.equal(calls[1].init.headers.get("session-id"), "session");
  assert.equal(calls[1].init.headers.get("thread-id"), "thread");
  assert.equal(calls[1].init.headers.get("chatgpt-account-id"), null);
  await assert.rejects(host.request("thread", encoded, "provider-secret"));
  assert.equal(calls.length, 2);
});

test("history failures redact provider bodies and cancellation aborts only its thread", async () => {
  const rejected = historyNotesHost({ direct: true, fetch: async () => new Response("private-token", { status: 401 }) });
  assert.deepEqual(JSON.parse(await rejected.request("thread", JSON.stringify(request), "test-token")), { status: 401, body: null });
  let signal;
  let started;
  const ready = new Promise((resolve) => { started = resolve; });
  const pending = historyNotesHost({ direct: true, fetch: async (_url, init) => {
    signal = init.signal;
    started();
    return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("private-token")), { once: true }));
  } });
  const result = pending.request("thread", JSON.stringify(request), "test-token");
  await ready;
  pending.cancel("another-thread");
  assert.equal(signal.aborted, false);
  pending.cancel("thread");
  await assert.rejects(result, (error) => /request failed/.test(error.message) && !/private-token/.test(error.message));
});

test("same-origin context proxy rejects alternate origins", () => {
  const location = { href: "https://nanocodex.localhost/" };
  assert.equal(sameOriginHistoryNotes("wss://evil.example/api/responses", location), undefined);
  assert.equal(sameOriginHistoryNotes("wss://nanocodex.localhost/api/responses?extra", location), undefined);
  assert.ok(sameOriginHistoryNotes("wss://nanocodex.localhost/api/responses", location));
});
