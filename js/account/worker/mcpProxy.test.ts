import assert from "node:assert/strict";
import { test } from "node:test";

import { proxyDefaultMcp } from "./mcpProxy.ts";

const THREAD_ID = "11111111-1111-4111-8111-111111111111";

test("default MCP proxy is same-origin, allowlisted, and uses the thread egress binding", async () => {
  const seen: Request[] = [];
  const egress = {
    async fetch(input: RequestInfo | URL, init?: RequestInit) {
      seen.push(new Request(input, init));
      return new Response("event: message\ndata: {}\n\n", {
      headers: {
        "content-type": "text/event-stream",
        "mcp-session-id": "session-2",
        "set-cookie": "must-not-leak=1",
      },
      });
    },
  } as Fetcher;
  const url = new URL(`https://demo.test/api/mcp/cloudflare?thread_id=${THREAD_ID}&cursor=next`);
    const response = await proxyDefaultMcp(new Request(url, {
      method: "POST",
      headers: {
        authorization: "must-not-forward",
        "content-type": "application/json",
        "mcp-protocol-version": "2025-11-25",
        "mcp-session-id": "session-1",
      },
      body: "{}",
    }), url, true, egress);

    assert.equal(response?.status, 200);
    assert.equal(response?.headers.get("content-type"), "text/event-stream");
    assert.equal(response?.headers.get("mcp-session-id"), "session-2");
    assert.equal(response?.headers.get("set-cookie"), null);
    assert.equal(seen[0]?.url, "https://demo.test/v1/egress");
    assert.equal(seen[0]?.headers.get("authorization"), null);
    const envelope = await seen[0]?.json() as Record<string, unknown>;
    assert.equal(envelope.thread_id, THREAD_ID);
    assert.equal(envelope.url, "https://docs.mcp.cloudflare.com/mcp?cursor=next");
    assert.equal(envelope.method, "POST");
    assert.equal(envelope.body, "{}");
    assert.deepEqual(envelope.headers, {
      "content-type": "application/json",
      "mcp-protocol-version": "2025-11-25",
      "mcp-session-id": "session-1",
    });
});

test("default MCP proxy rejects forged origins, unknown servers, and methods", async () => {
  const url = new URL(`https://demo.test/api/mcp/cloudflare?thread_id=${THREAD_ID}`);
  assert.equal((await proxyDefaultMcp(new Request(url), url, false))?.status, 403);

  const unknown = new URL("https://demo.test/api/mcp/arbitrary");
  assert.equal((await proxyDefaultMcp(new Request(unknown), unknown, true))?.status, 404);

  assert.equal((await proxyDefaultMcp(new Request(url, { method: "PUT" }), url, true))?.status, 405);
  assert.equal(await proxyDefaultMcp(new Request("https://demo.test/api/other"), new URL("https://demo.test/api/other"), true), undefined);
});

test("default MCP proxy adds no application byte ceiling", async () => {
  const seen: Request[] = [];
  const egress = {
    async fetch(input: RequestInfo | URL, init?: RequestInit) {
      seen.push(new Request(input, init));
      return new Response();
    },
  } as Fetcher;
    const url = new URL(`https://demo.test/api/mcp/cloudflare?thread_id=${THREAD_ID}`);
    const response = await proxyDefaultMcp(new Request(url, {
      method: "POST",
      headers: { "content-length": String(Number.MAX_SAFE_INTEGER) },
      body: "{}",
    }), url, true, egress);

    assert.equal(response?.status, 200);
    assert.equal(seen.length, 1);
});
