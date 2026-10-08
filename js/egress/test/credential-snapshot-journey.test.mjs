import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

// Production broker, regional snapshot DO, sealed storage and RPC run in
// workerd with persisted state across a process restart. Only the OAuth
// provider, the per-region invalidation fault switch and the grant-reply
// delay gate are synthetic.
const require = createRequire(import.meta.url);
const wranglerRequire = createRequire(require.resolve("wrangler/package.json"));
const { build } = wranglerRequire("esbuild");
const { Miniflare, convertV4MiniflareOptions } = wranglerRequire("miniflare");
const directory = fileURLToPath(new URL("..", import.meta.url));
const repository = resolve(directory, "../..");
const output = join(repository, "output/egress-credential-snapshot-journey", `${Date.now()}-${process.pid}`);
const owner = "22222222-2222-4222-8222-222222222222";
const EARLY_MS = 5 * 60_000;
const sleep = ms => new Promise(r => setTimeout(r, ms));
const jwt = value => `${Buffer.from('{"alg":"none"}').toString("base64url")}.${Buffer.from(JSON.stringify(value)).toString("base64url")}.fixture`;

test("regional credential lease: revocation ACK, delayed grant race, expiry after await, retry, restart, zero lease", { timeout: 90_000 }, async t => {
  await mkdir(output, { recursive: true });
  const bundle = await build({
    entryPoints: [join(directory, "test/fixtures/credential-snapshot-entry.ts")], bundle: true, write: false,
    format: "esm", platform: "node", target: "es2022", external: ["cloudflare:*"], logLevel: "warning",
    alias: {
      "node-rsa": join(repository, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs"),
      "@whiskeysockets/baileys": join(directory, "src/whatsapp-generated/baileys.js"),
    },
    plugins: [{ name: "static-wasm", setup(builder) {
      builder.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: "./nanocodex.wasm", external: true }));
      builder.onResolve({ filter: /bridge\.wasm$/ }, () => ({ path: "./bridge.wasm", external: true }));
    } }],
  });
  const modules = [{ type: "ESModule", path: join(output, "egress.js"), contents: bundle.outputFiles[0].text },
    ...await Promise.all([
      ["nanocodex.wasm", join(repository, "js/nanocodex/pkg-web/nanocodex_bg.wasm")],
      ["bridge.wasm", join(directory, "src/whatsapp-generated/bridge.wasm")],
    ].map(async ([name, path]) => ({ type: "CompiledWasm", path: join(output, name), contents: await readFile(path) })))];
  const trace = [];
  const began = performance.now();
  const record = (kind, detail = {}) => trace.push({ kind, ...detail, elapsed_ms: Math.round(performance.now() - began) });
  const failing = new Set();
  let invalidations = 0, refreshes = 0;
  const gates = new Map();
  const holdGrant = region => {
    let arrived, release;
    const reached = new Promise(r => { arrived = r; });
    const released = new Promise(r => { release = r; });
    gates.set(region, { arrived, released });
    return { reached, release };
  };
  const provider = async request => {
    const url = new URL(request.url);
    if (url.hostname === "fault.fixture" && url.pathname.startsWith("/grant/")) {
      const region = url.pathname.split("/").pop();
      const gate = gates.get(region);
      if (gate) {
        gates.delete(region);
        record("grant_reply_held", { region });
        gate.arrived();
        await gate.released;
        record("grant_reply_released", { region });
      }
      return new Response(null, { status: 200 });
    }
    if (url.hostname === "fault.fixture") {
      const region = url.pathname.split("/").pop();
      invalidations++;
      record("invalidate", { region, injected_failure: failing.has(region) });
      return new Response(null, { status: failing.has(region) ? 500 : 200 });
    }
    if (url.pathname === "/oauth/token") {
      refreshes++;
      record("refresh_rate_limited", { attempt: refreshes });
      return Response.json({ error: "rate_limited" }, { status: 429, headers: { "retry-after": "600" } });
    }
    return new Response("unexpected outbound request", { status: 599 });
  };
  const runtime = { modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"] };
  const options = convertV4MiniflareOptions({ workers: [
    { ...runtime, name: "gateway", script: `export default { fetch(request, env) {
      const path = new URL(request.url).pathname;
      if (path.startsWith('/users/')) return env.EGRESS.fetch(new Request('https://broker.internal' + path, request));
      return env.EGRESS.fetch(new Request('https://nanocodex.internal' + path, request));
    } };`, serviceBindings: { EGRESS: "egress" } },
    { ...runtime, name: "egress", modules, bindings: { ENVIRONMENT: "test", CREDENTIAL_ENCRYPTION_KEY: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY" },
      durableObjects: {
        USER_CREDENTIALS: { className: "UserCredentialBroker", useSQLite: true },
        USER_CREDENTIAL_SNAPSHOTS: { className: "FaultInjectedSnapshot", useSQLite: true },
      }, outboundService: provider },
  ] });
  options.resourcePersistencePath = join(output, "state");
  let mf = new Miniflare(options);
  t.after(async () => {
    await mf.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({
      command: "node --test js/egress/test/credential-snapshot-journey.test.mjs", invalidations, refreshes, trace,
    }, null, 2) + "\n");
    t.diagnostic(`Evidence: ${output}/trace.json`);
  });
  let endpoint = await mf.ready;
  const control = async (method, kind, value) => {
    const response = await fetch(new URL(`/users/${owner}/credentials/${kind}`, endpoint), { method,
      ...(value === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(value) }) });
    const body = await response.text();
    record("control", { method, kind, status: response.status, code: response.status >= 400 ? body.slice(0, 80) : undefined });
    return { status: response.status, body };
  };
  const regional = async (region = "weur") => {
    const result = await (await fetch(new URL(`/__test/resolve/${region}/${owner}`, endpoint))).json();
    record("regional_resolve", { region, status: result.status, source: result.source, has_secret: result.secret !== null });
    return result;
  };
  const shortChatGpt = (marker, extraMs) => {
    const exp = Math.ceil((Date.now() + EARLY_MS + extraMs) / 1000);
    return { access_token: jwt({ exp, marker, "https://api.openai.com/auth": { chatgpt_account_id: "synthetic-account", chatgpt_account_is_fedramp: false } }),
      refresh_token: "synthetic-refresh", account_id: "synthetic-account", expires_at: exp * 1000, fedramp: false };
  };

  // 1. Warm fill, local hit, and rotation invalidates before the 204.
  assert.equal((await control("PUT", "openai", { api_key: "sk-synthetic-a" })).status, 204);
  let result = await regional();
  assert.deepEqual([result.status, result.source, result.secret], [200, "filled", "sk-synthetic-a"]);
  result = await regional();
  assert.deepEqual([result.status, result.source, result.secret], [200, "snapshot", "sk-synthetic-a"]);
  const beforeRotation = invalidations;
  assert.equal((await control("PUT", "openai", { api_key: "sk-synthetic-b" })).status, 204);
  assert.equal(invalidations, beforeRotation + 1, "rotation acknowledged without holder ACK");
  result = await regional();
  assert.deepEqual([result.status, result.source, result.secret], [200, "filled", "sk-synthetic-b"]);
  assert.equal((await control("DELETE", "openai")).status, 204);
  record("rotation_revoked");

  // 1b. Delayed canonical grant race. A first-time region's grant is
  // registered by the broker, then its reply is held in transit while a
  // rotation completes. The rotation must wait for that holder's ACK, and the
  // late old grant must never be served or cached.
  assert.equal((await control("PUT", "openai", { api_key: "sk-race-old" })).status, 204);
  const gate = holdGrant("eeur");
  const racing = regional("eeur");
  await gate.reached;
  const beforeRace = trace.filter(e => e.kind === "invalidate" && e.region === "eeur").length;
  assert.equal((await control("PUT", "openai", { api_key: "sk-race-new" })).status, 204);
  assert.equal(trace.filter(e => e.kind === "invalidate" && e.region === "eeur").length, beforeRace + 1,
    "rotation acknowledged without ACK from the in-flight grant holder");
  gate.release();
  result = await racing;
  assert.notEqual(result.secret, "sk-race-old", "late pre-rotation grant served");
  assert.deepEqual([result.status, result.source, result.secret], [200, "filled", "sk-race-new"]);
  result = await regional("eeur");
  assert.deepEqual([result.status, result.source, result.secret], [200, "snapshot", "sk-race-new"]);
  assert.equal((await control("DELETE", "openai")).status, 204);
  record("delayed_grant_fenced");


  // 2. A grant whose lease would be zero (token inside refresh-early window,
  // refresh backoff) must never hand the credential to the replica.
  assert.equal((await control("PUT", "chatgpt", shortChatGpt("short", 3_000))).status, 204);
  result = await regional();
  assert.equal(result.status, 200); assert.equal(result.source, "filled");
  const shortSecret = result.secret;
  await sleep(4_500);
  result = await regional();
  assert.equal(result.status, 503, "zero-duration grant returned a credential");
  assert.equal(result.secret, null);
  assert.ok(refreshes >= 1, "refresh backoff branch not exercised");
  record("zero_lease_refused");

  // 3. Holder's lease is long expired (past old expiry+skew pruning). With
  // its invalidation failing, every mutation attempt, including idempotent
  // retries and after a workerd restart, must fail until the holder ACKs.
  failing.add("weur");
  await sleep(6_000);
  let removal = await control("DELETE", "chatgpt");
  assert.equal(removal.status, 503, "expired holder pruned without invalidation ACK");
  assert.match(removal.body, /credential_revocation_pending/);
  removal = await control("DELETE", "chatgpt");
  assert.equal(removal.status, 503, "idempotent retry acknowledged while revocation pending");
  await mf.dispose();
  mf = new Miniflare(options);
  endpoint = await mf.ready;
  record("workerd_restarted");
  removal = await control("DELETE", "chatgpt");
  assert.equal(removal.status, 503, "restart forgot pending revocation");
  result = await regional();
  assert.equal(result.secret, null, "replica served while revocation pending");
  assert.notEqual(result.status, 200);
  failing.delete("weur");
  removal = await control("DELETE", "chatgpt");
  assert.equal(removal.status, 204, removal.body);
  result = await regional();
  assert.equal(result.secret, null);
  assert.notEqual(result.secret, shortSecret);
  record("pending_revocation_acknowledged");

  // 4. After the ACK the region fills fresh state again.
  assert.equal((await control("PUT", "openai", { api_key: "sk-synthetic-c" })).status, 204);
  result = await regional();
  assert.deepEqual([result.status, result.secret], [200, "sk-synthetic-c"]);
  record("refilled_after_ack");

  // 5. Expiry after await. The grant carries a positive (~3 s) lease, but
  // its reply is held past both lease and credential refresh window. The
  // filled response must be refused, not served from the stale grant.
  assert.equal((await control("DELETE", "openai")).status, 204);
  assert.equal((await control("PUT", "chatgpt", shortChatGpt("held", 3_000))).status, 204);
  const held = holdGrant("apac");
  const expiring = regional("apac");
  await held.reached;
  await sleep(4_500);
  held.release();
  result = await expiring;
  assert.equal(result.secret, null, "grant served after its lease expired in transit");
  assert.notEqual(result.status, 200);
  result = await regional("apac");
  assert.equal(result.secret, null, "expired grant was cached");
  assert.equal(trace.filter(e => e.kind === "grant_reply_held" && e.region === "apac").length, 1);
  record("expired_grant_refused");
});
