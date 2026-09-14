import assert from "node:assert/strict";
import { test } from "node:test";
import { proxyX, type XProxyEnv } from "./xProxy.ts";

const url = "https://demo.test/api/tools/x/browse?resource=profile&handle=gakonst&format=json";

test("browser X proxy forwards only the public query and propagates cancellation", async () => {
  const controller = new AbortController();
  let forwarded: Request | undefined;
  const env = {
    ENVIRONMENT: "development",
    NANOCODEX_X: { fetch: async (request: Request) => {
      forwarded = request;
      return Response.json({ markdown: "public post" }, { headers: { "set-cookie": "private=1" } });
    } } as Pick<Fetcher, "fetch">,
  };
  const response = await proxyX(new Request(url, {
    headers: { authorization: "Bearer private", cookie: "private=1", "x-nanocodex-connector-connection": "private" },
    signal: controller.signal,
  }), env, true);
  assert.equal(response?.status, 200);
  assert.deepEqual(await response?.json(), { markdown: "public post" });
  assert.equal(response?.headers.get("set-cookie"), null);
  assert.equal(forwarded?.url, "https://x.internal/api/browse?resource=profile&handle=gakonst&format=json");
  assert.deepEqual([...forwarded!.headers], [["accept", "application/json"]]);
  controller.abort();
  assert.equal(forwarded?.signal.aborted, true);
});

test("browser X proxy fences origins, routes, methods, and missing bindings", async () => {
  const env = { ENVIRONMENT: "development", NANOCODEX_X: { fetch: async () => assert.fail("must not fetch") } };
  assert.equal((await proxyX(new Request(url), env, false))?.status, 403);
  assert.equal((await proxyX(new Request("https://demo.test/api/tools/x/arbitrary"), env, true))?.status, 404);
  assert.equal((await proxyX(new Request(url, { method: "POST" }), env, true))?.status, 405);
  assert.equal((await proxyX(new Request(url), { ENVIRONMENT: "development" }, true))?.status, 503);
  assert.equal(await proxyX(new Request("https://demo.test/api/other"), env, true), undefined);
});

test("browser X proxy enforces production rate limits before the Worker call", async () => {
  const keys: string[] = [];
  const env: XProxyEnv = {
    ENVIRONMENT: "production",
    NANOCODEX_X: { fetch: async () => assert.fail("must not fetch") },
    AGENT_TOOL_LIMIT: { limit: async ({ key }) => { keys.push(key); return { success: false }; } },
  };
  const response = await proxyX(new Request(url, { headers: { "cf-connecting-ip": "203.0.113.1" } }), env, true);
  assert.equal(response?.status, 429);
  assert.equal(response?.headers.get("retry-after"), "60");
  assert.equal(keys.length, 1);
  assert.doesNotMatch(keys[0]!, /203\.0\.113/);
  assert.equal((await proxyX(new Request(url), { ...env, AGENT_TOOL_LIMIT: undefined }, true))?.status, 503);
});

test("browser X proxy preserves upstream failures and handles binding failures", async () => {
  const env = {
    ENVIRONMENT: "development",
    NANOCODEX_X: { fetch: async () => Response.json({ error: "rate limited", retry_after: 60 }, {
      status: 429, headers: { "retry-after": "60" },
    }) },
  };
  const response = await proxyX(new Request(url), env, true);
  assert.equal(response?.status, 429);
  assert.equal(response?.headers.get("retry-after"), "60");
  assert.deepEqual(await response?.json(), { error: "rate limited", retry_after: 60 });
  assert.equal((await proxyX(new Request(url, { method: "HEAD" }), env, true))?.body, null);
  env.NANOCODEX_X = { fetch: async () => { throw new Error("internal detail"); } };
  const failed = await proxyX(new Request(url), env, true);
  assert.equal(failed?.status, 502);
  assert.deepEqual(await failed?.json(), { error: "X service unavailable" });
});
