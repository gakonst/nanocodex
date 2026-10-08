import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";

const root = fileURLToPath(new URL("..", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000071";
const thread = "00000000-0000-7000-8000-000000000072";
const machine = "synthetic-rehomed-hand";
const credential = "Bearer synthetic-account-placement";
const home = `~home/v1/wnam/${owner}`;
const command = "pnpm --filter nanocodex-managed-service exec node --test test/account-placement-journey.test.mjs";

// Real account objects, eager adoption, export/import, wipe and native shell.
// Fixtures: admission, a hit log on the previous object, and one export fault.
const source = `
import { AccountHostedTools as Base } from './src/account-hosted-tools.ts';
import { accountTools, accountObjectStub } from './src/account-placement.ts';
let failRows = false;
export class AccountHostedTools extends Base {
  constructor(ctx, env) { super(ctx, env); this.fixtureName = ctx.id.name; }
  #hit(kind) { if (this.fixtureName === '${owner}') console.info(JSON.stringify({ type: 'fixture.previous_hit', kind })); }
  async fetch(request) { this.#hit('fetch:' + new URL(request.url).pathname); return super.fetch(request); }
  async releaseManifest(...args) { this.#hit('releaseManifest'); return super.releaseManifest(...args); }
  async releaseRows(...args) { this.#hit('releaseRows'); if (failRows) throw new Error('fixture_export_unreachable'); return super.releaseRows(...args); }
  async wipeRetiredAccount(...args) { this.#hit('wipe'); return super.wipeRetiredAccount(...args); }
}
export default { async fetch(request, env) {
  if (request.headers.get('authorization') !== '${credential}') return new Response(null, {status:401});
  const url = new URL(request.url);
  if (url.pathname === '/__fixture/fail-rows') { failRows = (await request.json()).fail; return Response.json({ ok: true }); }
  if (url.pathname === '/__fixture/previous') {
    // Direct inspection of the previous object, bypassing routing.
    const stub = accountObjectStub(env.NANOCODEX_ACCOUNT_TOOLS, '${owner}');
    const response = await stub.fetch('https://account-tools.internal/snapshot', { method: 'POST', body: JSON.stringify({ owner_id: '${owner}' }) });
    let manifest; try { manifest = await stub.releaseManifest('${owner}', '${home}'); } catch (error) { manifest = { error: error.message }; }
    return Response.json({ status: response.status, body: await response.text(), manifest });
  }
  if (url.pathname === '/__fixture/forget') return Response.json(await accountTools(env).getByName('${owner}').forgetMachine('${owner}', '${machine}', true));
  return accountTools(env).getByName('${owner}').fetch(request);
} };
`;

test("a pinned account adopts eagerly, wipes its previous object and never addresses it again", { timeout: 120_000 }, async () => {
  const output = join(root, "../../output/account-placement-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand"), persist = join(output, "sqlite");
  await mkdir(workspace, { recursive: true });
  const runtime = [], wire = [], sockets = [], observed = {};
  let mf, connector, failure, base, script;
  const waitFor = async (predicate, description, ms = 15_000) => {
    const deadline = performance.now() + ms;
    while (performance.now() < deadline) { try { const value = await predicate(); if (value) return value; } catch { /* retry */ } await delay(25); }
    throw Error(`${description} exceeded ${ms}ms`);
  };
  const request = async (path, body) => {
    const response = await fetch(new URL(path, base), { method: body === undefined ? "GET" : "POST",
      headers: { authorization: credential, "x-nanocodex-owner-id": owner, "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(15_000) });
    const text = await response.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
    return { status: response.status, value };
  };
  const api = async (path, body) => { const r = await request(path, body); assert.equal(r.status, 200, JSON.stringify({ path, ...r })); return r.value; };
  const snapshot = () => api("/snapshot", { owner_id: owner });
  const route = state => state.machines.find(entry => entry.machine.id === machine)?.tools.find(tool => tool.name === "exec_command");
  const online = async () => { const state = await snapshot(); return state.machines.find(e => e.machine.id === machine)?.online ? state : undefined; };
  const invoke = (callId, routeToken, cmd) => api("/invoke", { owner_id: owner, name: "exec_command", machine_id: machine,
    session_id: thread, thread_id: thread, call_id: callId, model: "synthetic-model", route_token: routeToken,
    input: { cmd, shell: "/bin/sh", login: false, yield_time_ms: 30_000 } });
  const previousHits = () => runtime.join("").split('"fixture.previous_hit"').length - 1;
  const start = async homes => {
    mf = new Miniflare({ modules: true, script, compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      durableObjects: { NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true } }, durableObjectsPersist: persist,
      bindings: homes ? { NANOCODEX_ACCOUNT_HOMES: homes } : {},
      handleRuntimeStdio(stdout, stderr) { for (const s of [stdout, stderr]) s.on("data", chunk => runtime.push(String(chunk))); } });
    base = await mf.ready;
  };
  try {
    const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
      format: "esm", platform: "node", target: "es2022", conditions: ["workerd"],
      banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
      alias: { "node-rsa": root + "/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
      external: ["cloudflare:*", "node:*"], logLevel: "warning" });
    script = bundle.outputFiles[0].text;
    await writeFile(join(output, "worker.mjs"), script);
    // Phase 1: unpinned account lives in its original object.
    await start();
    const native = await createNodeProcessTools({ workspace });
    const tools = await createTools({ tools: native.tools });
    connector = createAttachment(tools, { endpoint: "ws://127.0.0.1/tool-host", transport: { connect() {
      const endpoint = new URL("/tool-host", base); endpoint.protocol = "ws:";
      const attempt = sockets.length + 1;
      const socket = new WebSocket(endpoint, { headers: { authorization: credential, "x-nanocodex-owner-id": owner } });
      sockets.push(socket); wire.push({ attempt, event: "connect", at: Date.now() });
      socket.on("close", (code, reason) => wire.push({ attempt, event: "close", at: Date.now(), code, reason: String(reason) }));
      socket.on("error", error => wire.push({ attempt, event: "error", at: Date.now(), error: error.message }));
      return socket;
    } } }, { machines: [{ id: machine, name: "Synthetic re-homed Hand", workspace, capabilities: ["shell"] }],
      attachmentId: machine, heartbeatMs: 100, reconnectDelayMs: 50, drainTimeoutMs: 1000 });
    await connector.connect();
    const initial = await waitFor(online, "original publication");
    const oldRoute = route(initial);
    assert.ok(oldRoute?.route_token);
    assert.match(JSON.stringify(await invoke("call-before", oldRoute.route_token, "printf A >> effects.log; printf BEFORE_OK")), /BEFORE_OK/);

    // Phase 2 ("deploy"): the pin is set and the export path is unreachable.
    // The home fails closed: no request is served by, or forwarded to, the old object.
    await mf.dispose();
    const phase2 = runtime.join("").length;
    await start(`${owner}:wnam`);
    await api("/__fixture/fail-rows", { fail: true });
    const refused = await request("/snapshot", { owner_id: owner });
    assert.notEqual(refused.status, 200, JSON.stringify(refused));
    assert.ok(!runtime.join("").slice(phase2).includes('"kind":"fetch:'), "no request was routed to the old object");
    observed.failed_adoption_fails_closed = { status: refused.status };

    // Export reachable again: the next activation adopts eagerly before any event.
    await api("/__fixture/fail-rows", { fail: false });
    const after = await waitFor(online, "Hand reconnects straight to the adopted home");
    assert.equal(route(after).route_token, oldRoute.route_token, "route identity copied");
    assert.deepEqual(after.mount_roots, initial.mount_roots);
    assert.ok(runtime.join("").includes("account.placement.adopted") && runtime.join("").includes("account.placement.wiped"));
    assert.match(JSON.stringify(await invoke("call-before", oldRoute.route_token, "printf A >> effects.log; printf BEFORE_OK")), /BEFORE_OK/);
    observed.adopted_and_wiped = true;

    // Normal operation after migration: zero requests reach the old object.
    const hits = previousHits();
    assert.match(JSON.stringify(await invoke("call-after", oldRoute.route_token, "printf B >> effects.log; printf AFTER_OK")), /AFTER_OK/);
    await snapshot();
    assert.equal(previousHits(), hits, "no request addressed the old object after migration");
    assert.equal(await readFile(join(workspace, "effects.log"), "utf8"), "AB", "receipt replay never re-executed");
    observed.zero_previous_requests = true;

    // The old object holds nothing and serves nothing.
    const previous = await api("/__fixture/previous");
    assert.equal(previous.status, 503);
    assert.equal(previous.manifest.tables?.length ?? -1, 0, JSON.stringify(previous.manifest));
    assert.deepEqual(previous.manifest.kv, []);
    observed.previous_empty = true;

    // Revocation applies at the home.
    await api("/__fixture/forget");
    assert.ok(!(await snapshot()).machines.some(entry => entry.machine.id === machine && entry.online));
    observed.revocation_at_home = true;
  } catch (error) { failure = error; }
  finally {
    await connector?.close?.().catch(() => undefined);
    for (const socket of sockets) socket.terminate?.();
    await mf?.dispose().catch(() => undefined);
    await writeFile(join(output, "evidence.json"), JSON.stringify({ command, observed, wire, runtime: runtime.join("").slice(-30_000) }, null, 2));
  }
  if (failure) throw failure;
  console.log(JSON.stringify({ evidence: output, ...observed }));
});
