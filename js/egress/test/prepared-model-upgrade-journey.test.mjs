import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

// Production SessionModelEgress, regional UserCredentialSnapshot, broker and
// the managed PreparedModelUpgrade run in workerd. Only the model provider
// (a WebSocket fixture recording handshakes/frames/closes) is synthetic.
const require = createRequire(import.meta.url);
const wranglerRequire = createRequire(require.resolve("wrangler/package.json"));
const { build } = wranglerRequire("esbuild");
const { Miniflare, convertV4MiniflareOptions } = wranglerRequire("miniflare");
const directory = fileURLToPath(new URL("..", import.meta.url));
const repository = resolve(directory, "../..");
const output = join(repository, "output/egress-prepared-model-upgrade-journey", `${Date.now()}-${process.pid}`);
const owner = "33333333-3333-4333-8333-333333333333";
const sleep = ms => new Promise(r => setTimeout(r, ms));

test("prepared upgrade: ACK before writes, durable handoff, cancel, invalidation, expiry, one-shot authority", { timeout: 90_000 }, async t => {
  await mkdir(output, { recursive: true });
  const shared = { bundle: true, write: false, format: "esm", target: "es2022", external: ["cloudflare:*"], logLevel: "warning" };
  const egress = await build({ ...shared, platform: "node",
    stdin: { contents: await readFile(join(directory, "src/egress.ts"), "utf8") + `
      // Test scheduling gate only: credential issuance remains the real broker.
      export class JourneyCredentialBroker extends UserCredentialBroker {
        async grantModelCredentialLease(owner, region) {
          await this.env.GRANT_GATE.fetch('https://fixture.internal/grant', { method: 'POST' });
          return super.grantModelCredentialLease(owner, region);
        }
      }`, resolveDir: join(directory, "src"), loader: "ts" },
    alias: {
      "node-rsa": join(repository, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs"),
      "@whiskeysockets/baileys": join(directory, "src/whatsapp-generated/baileys.js"),
    },
    plugins: [{ name: "static-wasm", setup(builder) {
      builder.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: "./nanocodex.wasm", external: true }));
      builder.onResolve({ filter: /bridge\.wasm$/ }, () => ({ path: "./bridge.wasm", external: true }));
    } }],
  });
  const session = await build({ ...shared, platform: "neutral", entryPoints: [join(directory, "test/fixtures/prepared-upgrade-session.mjs")] });
  await writeFile(join(output, "session.js"), session.outputFiles[0].text);
  const modules = [{ type: "ESModule", path: join(output, "egress.js"), contents: egress.outputFiles[0].text },
    ...await Promise.all([
      ["nanocodex.wasm", join(repository, "js/nanocodex/pkg-web/nanocodex_bg.wasm")],
      ["bridge.wasm", join(directory, "src/whatsapp-generated/bridge.wasm")],
    ].map(async ([name, path]) => ({ type: "CompiledWasm", path: join(output, name), contents: await readFile(path) })))];
  const events = [], logs = [];
  let holdNextUpgrade = false, holdGrants = false;
  const grantReleases = [];
  const heldUpgrades = new Map();
  const began = performance.now();
  const trace = async request => {
    const event = await request.json();
    events.push({ ...event, elapsed_ms: Math.round(performance.now() - began) });
    if (event.event === "upgrade" && holdNextUpgrade) {
      holdNextUpgrade = false;
      // Delay the synthetic provider's handshake response, keeping the real
      // regional holder's production starter pending through cancellation.
      await new Promise(resolve => heldUpgrades.set(event.rid, resolve));
    }
    return new Response(null, { status: 204 });
  };
  const runtime = { modules: true, modulesRoot: repository, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"] };
  const options = convertV4MiniflareOptions({ workers: [
    { ...runtime, name: "gateway", script: `export default { fetch(request, env) {
      const url = new URL(request.url);
      if (request.headers.has("x-fixture-upgrade")) {
        request = new Request(request);
        request.headers.delete("x-fixture-upgrade");
        request.headers.set("upgrade", "websocket");
      }
      if (url.pathname.startsWith('/users/')) return env.EGRESS.fetch(new Request('https://broker.internal' + url.pathname, request));
      if (url.pathname === '/generic') return env.EGRESS.fetch(new Request('https://nanocodex.internal/v1/responses', request));
      if (url.pathname === '/model') return env.MODEL.fetch(new Request('https://nanocodex.internal/v1/responses', request));
      return env.SESSION.fetch(request);
    } };`, serviceBindings: { EGRESS: "egress", MODEL: { name: "egress", entrypoint: "SessionModelEgress" }, SESSION: "session" } },
    { ...runtime, name: "session", modules: [{ type: "ESModule", path: join(output, "session.js"), contents: session.outputFiles[0].text }],
      durableObjects: { SESSIONS: { className: "Session", useSQLite: true } },
      serviceBindings: { MODEL: { name: "egress", entrypoint: "SessionModelEgress" } } },
    { ...runtime, name: "egress", modules, bindings: { ENVIRONMENT: "test", CREDENTIAL_ENCRYPTION_KEY: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY" },
      durableObjects: {
        USER_CREDENTIALS: { className: "JourneyCredentialBroker", useSQLite: true },
        USER_CREDENTIAL_SNAPSHOTS: { className: "UserCredentialSnapshot", useSQLite: true },
      }, outboundService: "provider", serviceBindings: { GRANT_GATE: async () => {
        if (holdGrants) await new Promise(resolve => grantReleases.push(resolve));
        return new Response(null, { status: 204 });
      } } },
    { ...runtime, name: "provider", script: `export default { async fetch(request, env) {
      const record = event => env.TRACE.fetch('https://trace.internal/', { method: 'POST', body: JSON.stringify(event) });
      if (request.headers.get('upgrade')?.toLowerCase() !== 'websocket' || request.url !== 'https://api.openai.com/v1/responses')
        return new Response('unexpected outbound request', { status: 599 });
      const key = request.headers.get('authorization');
      const leaked = [...request.headers.keys()].filter(name => name.startsWith('x-nanocodex-'));
      const rid = request.headers.get('x-client-request-id');
      await record({ event: 'upgrade', key, rid, leaked });
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      server.addEventListener('message', event => {
        const frame = JSON.parse(event.data);
        void record({ event: 'frame', rid, input: frame.input });
        server.send(JSON.stringify({ type: 'response.completed', response: { id: 'resp_fixture', key } }));
      });
      server.addEventListener('close', () => { void record({ event: 'close', rid }); try { server.close(1000, 'bye'); } catch {} });
      return new Response(null, { status: 101, webSocket: client });
    } };`, serviceBindings: { TRACE: trace } },
  ] });
  options.handleStructuredLogs = ({ message }) => {
    if (message.includes("prepared_model_upgrade") || message.includes("model_upgrade_preparation")) logs.push(message);
  };
  const mf = new Miniflare(options);
  t.after(async () => {
    for (const release of heldUpgrades.values()) release();
    for (const release of grantReleases) release();
    await mf.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({ command: "node --test js/egress/test/prepared-model-upgrade-journey.test.mjs", events, logs }, null, 2) + "\n");
    t.diagnostic(`Evidence: ${output}/trace.json`);
  });
  const endpoint = await mf.ready;
  const call = async (path, init) => {
    const headers = new Headers(init?.headers);
    if (headers.has("upgrade")) { headers.delete("upgrade"); headers.set("x-fixture-upgrade", "1"); }
    return fetch(new URL(path, endpoint), { ...init, headers });
  };
  const json = async (path, init) => { const response = await call(path, init); return { status: response.status, body: await response.json() }; };
  const op = (id, name, query = "") => json(`/session/${name}?id=${id}${query}`);
  const control = async (method, value) => {
    const response = await call(`/users/${owner}/credentials/openai`, { method,
      ...(value === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(value) }) });
    assert.equal(response.status, 204, await response.text());
  };
  const of = rid => events.filter(event => event.rid === rid);
  const until = async (predicate, label, ms = 5000) => {
    for (const end = Date.now() + ms; Date.now() < end; await sleep(20)) if (predicate()) return;
    throw new Error(`${label} did not converge: ${JSON.stringify(events.slice(-6))}`);
  };
  const rid = prepared => new Map(prepared.body.headers).get("session-id");
  const upgrades = id => of(id).filter(event => event.event === "upgrade");
  await control("PUT", { api_key: "sk-key-1" });

  // Handoff: ACK before writes; handshake completes while durability is held;
  // the exact prepared socket is consumed once and carries the first frame.
  await op("handoff", "hold");
  const handoff = await op("handoff", "prepare");
  assert.equal(handoff.status, 200);
  const handoffRid = rid(handoff);
  assert.ok(handoff.body.ack_ms < 1000, `ACK took ${handoff.body.ack_ms}ms`);
  await until(() => upgrades(handoffRid).length === 1, "prepared handshake");
  const taking = op("handoff", "take");
  await sleep(300);
  assert.equal(of(handoffRid).filter(event => event.event === "frame").length, 0, "frame before durable admission");
  // A guessed id with exact Session authority is refused and does not disturb the pending handshake.
  const guessed = await call("/model", { headers: { ...Object.fromEntries(handoff.body.headers),
    "x-nanocodex-prepared-model-upgrade": crypto.randomUUID() } });
  assert.equal(guessed.status, 404); await guessed.body?.cancel();
  await op("handoff", "release");
  const handed = await taking;
  assert.equal(handed.status, 200, JSON.stringify(handed.body));
  assert.equal(handed.body.prepared, true, "prepared socket was not consumed");
  assert.equal(handed.body.frames.at(-1).response.key, "Bearer sk-key-1");
  await until(() => of(handoffRid).some(event => event.event === "frame"), "handoff frame");
  assert.equal(upgrades(handoffRid).length, 1, "handoff opened a second connection");
  assert.deepEqual(upgrades(handoffRid)[0].leaked, [], "private headers reached the provider");

  // Cancellation: Session retirement releases the remote handshake without frames.
  const cancel = await op("cancel", "prepare");
  const cancelRid = rid(cancel);
  await until(() => upgrades(cancelRid).length === 1, "cancel handshake");
  await op("cancel", "dispose");
  await until(() => of(cancelRid).some(event => event.event === "close"), "remote cancellation close");
  assert.equal((await op("cancel", "take")).status, 409);
  await sleep(200);
  assert.deepEqual(of(cancelRid).map(event => event.event), ["upgrade", "close"], "retired preparation sent a frame or fallback");

  // Cancellation while the production provider fetch is still pending must
  // settle the managed call without releasing that fetch or sending a frame.
  holdNextUpgrade = true;
  const stalled = await op("stalled", "prepare");
  const stalledRid = rid(stalled);
  await until(() => heldUpgrades.has(stalledRid), "held managed handshake");
  let stalledSettled = false;
  const stalledTake = op("stalled", "take").then(result => { stalledSettled = true; return result; });
  await sleep(100);
  assert.equal(stalledSettled, false);
  await op("stalled", "dispose");
  await until(() => stalledSettled, "cancelled managed consume", 1000);
  assert.equal((await stalledTake).status, 409);
  assert.deepEqual(of(stalledRid).map(event => event.event), ["upgrade"]);
  heldUpgrades.get(stalledRid)();
  heldUpgrades.delete(stalledRid);

  // Independently prove the holder's pending consume settles on cancel;
  // the managed helper's local cancellation race cannot mask a stuck fetch.
  holdNextUpgrade = true;
  const remote = await op("remote-stalled", "remote-prepare");
  assert.equal(remote.body.status, "prepared");
  const remoteRid = rid(remote);
  await until(() => heldUpgrades.has(remoteRid), "held remote handshake");
  let remoteSettled = false;
  const remoteTake = op("remote-stalled", "remote-take").then(result => { remoteSettled = true; return result; });
  await sleep(100);
  assert.equal(remoteSettled, false);
  assert.equal((await op("remote-stalled", "remote-cancel")).body.cancelled, true);
  await until(() => remoteSettled, "cancelled remote consume", 1000);
  assert.equal((await remoteTake).body.status, 404);
  assert.deepEqual(of(remoteRid).map(event => event.event), ["upgrade"]);
  heldUpgrades.get(remoteRid)();
  heldUpgrades.delete(remoteRid);

  // A held credential RPC cannot be aborted by fetch cancellation. The real
  // broker issues its actual lease only when this test scheduling gate opens.
  await control("PUT", { api_key: "sk-key-1" });
  holdGrants = true;
  const capacityRids = [];
  for (let index = 0; index < 8; index++) {
    const pending = await op(`capacity-${index}`, "remote-prepare");
    assert.equal(pending.body.status, "prepared", JSON.stringify(pending.body));
    capacityRids.push(rid(pending));
    await until(() => grantReleases.length > 0, "held credential grant");
    let settled = false;
    const consume = op(`capacity-${index}`, "remote-take").then(result => { settled = true; return result; });
    await sleep(50);
    assert.equal(settled, false);
    assert.equal((await op(`capacity-${index}`, "remote-cancel")).body.cancelled, true);
    await until(() => settled, "grant-held remote cancellation", 1000);
    assert.equal((await consume).body.status, 404);
    assert.equal(upgrades(rid(pending)).length, 0);
  }
  const busy = await op("capacity-busy", "remote-prepare");
  assert.equal(busy.body.status, "busy");
  assert.equal(upgrades(rid(busy)).length, 0);
  holdGrants = false;
  for (const release of grantReleases) release();
  await sleep(100);
  const recovered = await op("capacity-recovered", "remote-prepare");
  assert.equal(recovered.body.status, "prepared");
  await op("capacity-recovered", "remote-cancel");

  // Mismatch: a changed header set disposes the preparation and uses the ordinary path once.
  const mismatch = await op("mismatch", "prepare");
  const mismatchRid = rid(mismatch);
  await until(() => upgrades(mismatchRid).length === 1, "mismatch handshake");
  const mismatched = await op("mismatch", "take", "&mismatch=1");
  assert.equal(mismatched.body.prepared, false);
  assert.equal(mismatched.body.status, 101);
  await until(() => of(mismatchRid).some(event => event.event === "close"), "mismatch cancellation");
  assert.equal(upgrades(mismatchRid).length, 2);

  // Invalidation: a credential mutation disposes handshakes authorized by the
  // superseded credential; the fallback uses only the new credential.
  const invalidated = await op("invalidate", "prepare");
  const invalidatedRid = rid(invalidated);
  await until(() => upgrades(invalidatedRid).length === 1, "invalidation handshake");
  await control("PUT", { api_key: "sk-key-2" });
  await until(() => of(invalidatedRid).some(event => event.event === "close"), "invalidation close");
  const afterInvalidation = await op("invalidate", "take");
  assert.equal(afterInvalidation.body.prepared, false);
  assert.equal(afterInvalidation.body.frames.at(-1).response.key, "Bearer sk-key-2");
  assert.deepEqual(upgrades(invalidatedRid).map(event => event.key), ["Bearer sk-key-1", "Bearer sk-key-2"]);

  // Revocation before preparation: no provider handshake, fail-closed fallback.
  await control("DELETE");
  const revoked = await op("revoked", "prepare");
  const revokedTake = await op("revoked", "take");
  assert.equal(revokedTake.body.prepared, false);
  assert.equal(revokedTake.body.status, 409);
  assert.equal(upgrades(rid(revoked)).length, 0);
  await control("PUT", { api_key: "sk-key-3" });

  // The generic broker never accepts the preparation header.
  const generic = await call("/generic", { headers: { upgrade: "websocket", "x-nanocodex-prepared-model-upgrade": crypto.randomUUID() } });
  assert.equal(generic.status, 403); await generic.body?.cancel();

  // Expiry: the 10s bound closes the holder's handshake; take falls back once.
  const expiry = await op("expiry", "prepare");
  const expiryRid = rid(expiry);
  await until(() => upgrades(expiryRid).length === 1, "expiry handshake");
  await sleep(10_300);
  await until(() => of(expiryRid).some(event => event.event === "close"), "expiry close");
  const expired = await op("expiry", "take");
  assert.equal(expired.body.prepared, false);
  assert.equal(expired.body.frames.at(-1).response.key, "Bearer sk-key-3");
  assert.equal(upgrades(expiryRid).length, 2);

  // Logs correlate by subject only: never the request id or provider headers.
  await sleep(100);
  for (const line of logs) {
    assert.ok(!line.includes(handoffRid) && !line.includes("sk-key") && !line.includes("NANOCODEX_PROVIDER_CREDENTIAL"), line);
  }
  if (logs.length) {
    assert.ok(logs.some(line => line.includes("acknowledged")), "local ACK milestone");
    assert.ok(logs.some(line => line.includes("egress.prepared_model_upgrade") && line.includes("consumed") && line.includes("managed-session-v1_")));
  }
});
