import assert from "node:assert/strict";
import test from "node:test";

import { isAccountFundingPath, routeAccountFunding } from "./accountFundingProxy.ts";

const origin = "https://nanocodex.test";
const address = "0x1111111111111111111111111111111111111111";

test("matches only exact MACH funding routes", () => {
  for (const path of [
    "/v1/machine-usd/config",
    "/v1/machine-usd/orders",
    "/v1/machine-usd/orders/order_123",
  ]) assert.equal(isAccountFundingPath(path), true, path);
  for (const path of [
    "/v1/machine-usd",
    "/v1/machine-usd/other",
    "/v1/machine-usd/orders/order/extra",
  ]) assert.equal(isAccountFundingPath(path), false, path);
});

test("creates an order only for the authenticated canonical account wallet", async () => {
  const upstream: Request[] = [];
  const request = new Request(`${origin}/v1/machine-usd/orders`, {
    method: "POST",
    headers: {
      cookie: "account_session=secret",
      "content-type": "application/json",
      "idempotency-key": "attempt-1",
    },
    body: JSON.stringify({
      order_token: "order-token",
      payment_mode: "hosted_checkout",
      usd_amount_cents: 500,
      wallet_address: "0x2222222222222222222222222222222222222222",
      ignored: "value",
    }),
  });
  const response = await routeAccountFunding(request, {
    NANOCODEX_BACKEND: {
      async fetch(sessionRequest) {
        assert.equal(new URL(sessionRequest.url).pathname, "/v1/me");
        assert.equal(sessionRequest.method, "GET");
        assert.equal(sessionRequest.headers.get("cookie"), "account_session=secret");
        return Response.json({
          authentication: "account_session",
          user: { id: "user-1", persistent: true, address },
        });
      },
    },
    NANOCODEX_CONNECT_API: {
      async fetch(upstreamRequest) {
        upstream.push(upstreamRequest);
        return Response.json({ ok: true }, { status: 201 });
      },
    },
  }, new URL(request.url));
  assert.equal(response?.status, 201);
  assert.equal(upstream.length, 1);
  assert.equal(upstream[0]?.headers.has("cookie"), false);
  assert.equal(upstream[0]?.headers.get("idempotency-key"), "attempt-1");
  assert.deepEqual(await upstream[0]?.json(), {
    order_token: "order-token",
    payment_mode: "hosted_checkout",
    usd_amount_cents: 500,
    wallet_address: address,
  });
});

test("rejects anonymous order creation without reaching the onramp", async () => {
  let called = false;
  const request = new Request(`${origin}/v1/machine-usd/orders`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
  const response = await routeAccountFunding(request, {
    NANOCODEX_BACKEND: { fetch: async () => Response.json({ authenticated: false }, { status: 401 }) },
    NANOCODEX_CONNECT_API: {
      fetch: async () => {
        called = true;
        return new Response();
      },
    },
  }, new URL(request.url));
  assert.equal(response?.status, 401);
  assert.equal(called, false);
});

test("forwards status bearer credentials without account cookies", async () => {
  let upstream: Request | undefined;
  const request = new Request(`${origin}/v1/machine-usd/orders/order_123`, {
    headers: { authorization: "Bearer order-token", cookie: "account_session=secret" },
  });
  const response = await routeAccountFunding(request, {
    ENVIRONMENT: "development",
    NANOCODEX_CONNECT_API: {
      fetch: async (value) => {
        upstream = value;
        return Response.json({ order: { status: "processing" } });
      },
    },
  }, new URL(request.url));
  assert.equal(response?.status, 200);
  assert.equal(upstream?.headers.get("authorization"), "Bearer order-token");
  assert.equal(upstream?.headers.has("cookie"), false);
  assert.equal(upstream?.headers.get("x-nanocodex-local-origin"), origin);
});
