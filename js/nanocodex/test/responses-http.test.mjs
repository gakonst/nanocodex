import assert from "node:assert/strict";
import test from "node:test";
import { createResponsesHttp } from "../runtime/responses-http.mjs";
import { cloudflareEgress } from "../cloudflare/egress.mjs";
import { bindHostSession, releaseHostSession, installHostBridge } from "../internal.mjs";

test("HTTPS pulls bytes before EOF and close cancels the reader and request", async () => {
  let signal;
  let cancelled = false;
  let source;
  const http = createResponsesHttp(async (_endpoint, _key, _session, _meta, _body, requestSignal) => {
    signal = requestSignal;
    return new Response(new ReadableStream({
      start(controller) { source = controller; },
      cancel() { cancelled = true; },
    }), { headers: { "x-reasoning-included": "true", "x-codex-turn-state": "sticky" } });
  });
  const handle = http.httpOpen("https://provider.test/responses", "key", "root", {}, "{}");
  assert.deepEqual(JSON.parse(await http.httpReady(handle)), {
    status: 200, reasoning_included: true, turn_state: "sticky",
  });
  const chunk = new TextEncoder().encode("data: partial\n\n");
  source.enqueue(chunk);
  assert.deepEqual(await http.httpNext(handle), chunk);
  assert.equal(signal.aborted, false);
  http.httpClose(handle);
  assert.equal(signal.aborted, true);
  assert.equal(cancelled, true);
  http.httpClose(handle);
});

test("close while awaiting headers aborts fetch and disposes a late response", async () => {
  let resolve;
  let signal;
  let cancelled = false;
  const http = createResponsesHttp((_endpoint, _key, _session, _meta, _body, requestSignal) => {
    signal = requestSignal;
    return new Promise((done) => { resolve = done; });
  });
  const handle = http.httpOpen("https://provider.test/responses", "key", "root", {}, "{}");
  const ready = http.httpReady(handle);
  await Promise.resolve();
  http.httpClose(handle);
  assert.equal(signal.aborted, true);
  resolve(new Response(new ReadableStream({ cancel() { cancelled = true; } })));
  await assert.rejects(ready, /cancelled/);
  assert.equal(cancelled, true);
});

test("HTTP rejection preserves status, body and retry delay for Rust policy", async () => {
  const http = createResponsesHttp(async () => new Response('{"error":"overloaded"}', {
    status: 429, headers: { "retry-after": "7" },
  }));
  const handle = http.httpOpen("https://provider.test/responses", "key", "root", {}, "{}");
  await assert.rejects(http.httpReady(handle), (error) => {
    assert.deepEqual(JSON.parse(error), { kind: "handshake_rejected", status: 429,
      body: '{"error":"overloaded"}', retry_after: 7 });
    return true;
  });
  http.dispose();
});

test("broker HTTPS keeps exact endpoint, placeholders and thread metadata", async () => {
  let captured;
  const response = new Response("data: {}\n\n");
  const endpoint = cloudflareEgress({ binding: { fetch: async (url, init) => {
    captured = { url, init }; return response;
  } } });
  const signal = new AbortController().signal;
  const request = { authorization: "host_managed", threadId: "child", turnState: "sticky", body: "{}", signal };
  assert.equal(await endpoint.createResponse(`${endpoint.apiBaseUrl}/responses`, "root", request), response);
  assert.equal(captured.init.method, "POST");
  assert.equal(captured.init.redirect, "manual");
  assert.equal(captured.init.signal, signal);
  assert.equal(captured.init.headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
  assert.equal(captured.init.headers.get("session-id"), "root");
  assert.equal(captured.init.headers.get("thread-id"), "child");
  assert.equal(captured.init.headers.get("x-codex-turn-state"), "sticky");
  assert.equal(captured.init.headers.get("content-type"), "application/json");
  await assert.rejects(endpoint.createResponse("https://evil.test/responses", "root", request), /unexpected/);
  await assert.rejects(endpoint.createResponse(`${endpoint.apiBaseUrl}/responses`, "root", { ...request, authorization: "bearer" }), /hostManaged/);
});

test("global bridge routes HTTPS by thread host and cleans up its handle", async () => {
  let captured;
  let cancelled = false;
  const host = createResponsesHttp(async (...args) => {
    captured = args;
    return new Response(new ReadableStream({
      start(controller) { controller.enqueue(new Uint8Array([1, 2])); },
      cancel() { cancelled = true; },
    }));
  });
  bindHostSession(host, "http-child");
  installHostBridge();
  try {
    const bridge = globalThis.nanocodexHost;
    const handle = bridge.httpOpen("https://provider.test/responses", "key", "account", true, "root", "http-child", "sticky", "{}");
    assert.equal(JSON.parse(await bridge.httpReady(handle)).status, 200);
    assert.equal(captured[2], "root");
    assert.deepEqual(captured[3], { accountId: "account", fedramp: true, threadId: "http-child", turnState: "sticky" });
    assert.deepEqual(await bridge.httpNext(handle), new Uint8Array([1, 2]));
    bridge.httpClose(handle);
    assert.equal(cancelled, true);
    bridge.httpClose(handle);
  } finally { releaseHostSession(host, "http-child"); host.dispose(); }
});
