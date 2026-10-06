import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { routeAccountFunding } from "../../account/worker/accountFundingProxy.ts";
import { decodeMachineUsdConfig, decodeFundingAttempt, classifyFundingOrder } from "../../account/src/walletFunding.ts";
import { machOnramp } from "../src/machOnramp.ts";

const account = `0x${"1".repeat(40)}`;
const other = `0x${"2".repeat(40)}`;
const id = `ord_${"3".repeat(32)}`;
const canonical = "https://nanocodex.gakonst.workers.dev";
const config = { chain_id: 4217, min_usd_amount_cents: 500, max_usd_amount_cents: 10000,
  token_address: "0x20c000000000000000000000f37de3740adec032", stripe_publishable_key: "pk_live_fixture" };
const context = { waitUntil() {} };

// Real HTTP transport through the shipped account handler and bundled Connect worker.
// Only account authentication and the external MACH payment service use synthetic fixtures.
test("account funding journey reaches MACH and preserves identity, capability, retries and issuance", async t => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-mach-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [
    new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url).pathname,
    "deploy", "--dry-run", "--config", "wrangler.jsonc", "--outdir", outdir,
  ], { cwd: new URL("..", import.meta.url), maxBuffer: 2 * 1024 * 1024 });
  const connect = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  const requests = [];
  let order;
  let original;
  let enabled = true;
  let selected = account;
  const identity = "0x7777777777777777777777777777777777777777";
  const env = { CONNECT_STATE: { idFromName: x => x, get: () => ({}) }, MACH_ONRAMP: {
    async fetch(request) {
      requests.push(request.clone());
      assert.equal(request.headers.has("cookie"), false);
      assert.equal(request.headers.has("origin"), false);
      assert.equal(request.headers.has("x-nanocodex-mach-relay-token"), false);
      const url = new URL(request.url);
      assert.equal(url.origin, "https://mach.internal");
      if (url.pathname === "/v1/config") return Response.json(config);
      if (url.pathname === "/v1/orders" && request.method === "POST") {
        if (!enabled) return Response.json({ error: { code: "onramp_disabled" } }, { status: 503 });
        const body = await request.json();
        const key = request.headers.get("idempotency-key");
        assert.equal(key, `nanocodex:${account}:fixture-retry`);
        assert.equal(body.wallet_address, account);
        assert.equal(body.checkout_return_url, undefined);
        const current = JSON.stringify({ key, body });
        if (original && original !== current) return Response.json({ error: "idempotency_conflict" }, { status: 409 });
        const created = !original;
        original = current;
        order ??= { id, wallet_address: account, usd_amount_cents: 500, mach_amount_atomics: 5_000_000,
          status: "requires_payment", issuance_status: "not_started", issuance_transaction_hash: null };
        return Response.json({ order, payment: { provider: "stripe", mode: "hosted_checkout",
          checkout_url: "https://checkout.stripe.com/c/pay/cs_test_fixture" } }, { status: created ? 201 : 200 });
      }
      if (url.pathname === `/v1/orders/${id}`) {
        if (request.headers.get("authorization") !== "Bearer fixture-capability") return Response.json({ error: "order_not_found" }, { status: 404 });
        return Response.json({ order });
      }
      assert.fail(`Unexpected MACH route: ${request.method} ${url.pathname}`);
    },
  } };
  if (process.env.MACH_RELAY_SOURCE) {
    // Optional companion-repository journey: the real relay implementation is
    // supplied explicitly, never downloaded or substituted by a mock relay.
    const relay = (await import(pathToFileURL(process.env.MACH_RELAY_SOURCE))).default;
    const service = env.MACH_ONRAMP;
    const originalFetch = globalThis.fetch;
    const token = "synthetic-cross-account-transport-token";
    t.after(() => { globalThis.fetch = originalFetch; });
    globalThis.fetch = (input, init) => {
      const target = input instanceof Request ? input : new Request(input, init);
      return new URL(target.url).origin === "https://relay.test"
        ? relay.fetch(target, { MACH_ONRAMP: service, MACH_ONRAMP_RELAY_TOKEN: token })
        : originalFetch(input, init);
    };
    delete env.MACH_ONRAMP;
    env.MACH_ONRAMP_RELAY_URL = "https://relay.test";
    env.MACH_ONRAMP_RELAY_TOKEN = token;
  }
  const accountEnv = {
    NANOCODEX_BACKEND: { async fetch(request) {
      const pathname = new URL(request.url).pathname;
      assert.ok(["/v1/me", "/v1/wallet"].includes(pathname));
      const cookie = request.headers.get("cookie");
      if (!cookie) return Response.json({}, { status: 401 });
      if (pathname === "/v1/wallet") return Response.json({ address: cookie === "fixture=a" ? selected : other, mode: "linked", created_at: 1 });
      return Response.json({ authentication: "account_session", user: { persistent: true, address: identity } });
    } },
    NANOCODEX_CONNECT_API: { fetch: request => connect.fetch(request, env, context) },
  };
  const server = createServer(async (incoming, outgoing) => {
    try {
      const chunks = [];
      for await (const chunk of incoming) chunks.push(chunk);
      const request = new Request(`${canonical}${incoming.url}`, { method: incoming.method,
        headers: incoming.headers, ...(incoming.method === "POST" ? { body: Buffer.concat(chunks) } : {}) });
      const result = await routeAccountFunding(request, accountEnv, new URL(request.url));
      outgoing.writeHead(result.status, Object.fromEntries(result.headers));
      outgoing.end(Buffer.from(await result.arrayBuffer()));
    } catch (error) { outgoing.writeHead(500); outgoing.end(String(error)); }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const base = `http://127.0.0.1:${server.address().port}/v1/machine-usd`;
  const create = (overrides = {}, cookie = "fixture=a", origin = canonical) => fetch(`${base}/orders`, {
    method: "POST", headers: { ...(cookie ? { cookie } : {}), origin, "content-type": "application/json", "idempotency-key": "fixture-retry" },
    body: JSON.stringify({ wallet_address: account, order_token: "fixture-capability", payment_mode: "hosted_checkout", usd_amount_cents: 500,
      checkout_return_url: "https://untrusted.example/", ...overrides }),
  });
  const read = (cookie = "fixture=a", token = "fixture-capability") => fetch(`${base}/orders/${id}`, { headers: { ...(cookie ? { cookie } : {}), authorization: `Bearer ${token}` } });
  let response = await fetch(`${base}/config`);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.equal(decodeMachineUsdConfig(await response.json()).onrampEnabled, true);
  assert.equal((await create({}, "")).status, 401);
  assert.equal((await create({}, "fixture=a", "https://attacker.example")).status, 403);
  assert.equal((await create({ usd_amount_cents: 499 })).status, 400);
  assert.equal((await create({ usd_amount_cents: 10001 })).status, 400);
  const beforeStale = requests.length;
  assert.equal((await create({ wallet_address: identity })).status, 409);
  assert.equal(requests.length, beforeStale, "identity address cannot accidentally receive linked-wallet funding");
  selected = other;
  assert.equal((await create()).status, 409);
  assert.equal(requests.length, beforeStale, "a concurrent wallet switch must not create a redirected order");
  selected = account;
  response = await create();
  assert.equal(response.status, 201);
  assert.equal(decodeFundingAttempt(await response.json(), "fixture-capability", account, 500).id, id);
  assert.equal((await create()).status, 200);
  assert.equal((await create({ usd_amount_cents: 600 })).status, 409);
  assert.equal((await read("fixture=b")).status, 404);
  assert.equal((await read("")).status, 401);
  assert.equal((await read("fixture=a", "wrong-capability")).status, 404);
  response = await read();
  assert.equal(classifyFundingOrder((await response.json()).order), "pending");
  // Payment success alone is never issuance success.
  order = { ...order, status: "issuing", payment_status: "succeeded" };
  assert.equal(classifyFundingOrder((await (await read()).json()).order), "pending");
  order = { ...order, status: "complete", issuance_transaction_hash: `0x${"4".repeat(64)}` };
  assert.equal((await read()).status, 404);
  order = { ...order, issuance_status: "fulfilled" };
  assert.equal(classifyFundingOrder((await (await read()).json()).order), "complete");
  order = { ...order, id: `ord_${"5".repeat(32)}` };
  assert.equal((await read()).status, 404);
  enabled = false;
  assert.equal((await create()).status, 503);
  const unbound = await connect.fetch(new Request(`${canonical}/v1/machine-usd/config`), { CONNECT_STATE: env.CONNECT_STATE }, context);
  assert.equal(unbound.status, 503);
  assert.equal((await unbound.json()).error, "machine_usd_unavailable");
  t.diagnostic(JSON.stringify({ config: "200", create: "201", replay: "200", conflict: "409", anonymous: "401", foreignWallet: "404", paymentOnly: "pending", malformedCompletion: "404", fulfilled: "complete", disabled: "503", privatePaths: [...new Set(requests.map(r => new URL(r.url).pathname))] }));
});

test("relay client fails closed and sends its credential only to the configured HTTPS origin", async t => {
  const originalFetch = globalThis.fetch;
  t.after(() => { globalThis.fetch = originalFetch; });
  const seen = [];
  globalThis.fetch = async request => { seen.push(request); return Response.json(config, { headers: { "set-cookie": "never=forward" } }); };
  const request = new Request(`${canonical}/v1/machine-usd/config`, { headers: { cookie: "private-account" } });
  const env = { MACH_ONRAMP_RELAY_URL: "https://relay.test", MACH_ONRAMP_RELAY_TOKEN: "synthetic-transport-secret-32-characters" };
  const response = await machOnramp(request, env, "/v1/config");
  assert.equal(response.status, 200);
  assert.equal(seen[0].url, "https://relay.test/v1/config");
  assert.equal(seen[0].headers.get("x-nanocodex-mach-relay-token"), env.MACH_ONRAMP_RELAY_TOKEN);
  assert.equal(seen[0].headers.has("cookie"), false);
  assert.equal(response.headers.has("set-cookie"), false);
  for (const url of ["http://relay.test", "https://relay.test/other", "https://user@relay.test", "https://relay.test/?target=x"]) {
    assert.equal((await machOnramp(request, { ...env, MACH_ONRAMP_RELAY_URL: url }, "/v1/config")).status, 503);
  }
  assert.equal((await machOnramp(request, env, "/v1/issuances")).status, 404);
  globalThis.fetch = async () => Response.redirect("https://other.test");
  assert.equal((await machOnramp(request, env, "/v1/config")).status, 502);
  globalThis.fetch = async () => { throw new Error("connection lost"); };
  assert.equal((await machOnramp(request, env, "/v1/config")).status, 502);
  assert.equal(seen.length, 1);
});
