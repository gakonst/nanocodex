import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
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
const command = "pnpm --filter nanocodex-managed-service exec node --test test/account-placement-journey.test.mjs";

// Real account objects, placement, export/import, routing and native shell.
// Only admission is a fixture; /__fixture/raw addresses one exact object.
const source = `
import { AccountHostedTools as Base } from './src/account-hosted-tools.ts';
import { accountTools, accountObjectStub, routedAccountName } from './src/account-placement.ts';
// Fault only: verification of an adoption can be made unreachable (ambiguous outcome).
let failVerify = false;
export class AccountHostedTools extends Base {
  async accountPlacement(id) { if (failVerify) throw new Error('fixture_unreachable'); return super.accountPlacement(id); }
}
export default { async fetch(request, env) {
  if (request.headers.get('authorization') !== '${credential}') return new Response(null, {status:401});
  const url = new URL(request.url);
  if (url.pathname === '/__fixture/rehome') {
    const { region } = await request.json();
    try { return Response.json({ moved: await accountTools(env).getByName('${owner}').rehomeAccount('${owner}', region), at: routedAccountName(env, '${owner}') }); }
    catch (error) { return Response.json({ error: error.message }, { status: 500 }); }
  }
  if (url.pathname === '/__fixture/fail-verify') { failVerify = (await request.json()).fail; return Response.json({ ok: true }); }
  if (url.pathname === '/__fixture/where') return Response.json({ at: routedAccountName(env, '${owner}') });
  if (url.pathname === '/__fixture/raw') {
    const { name, path, body, rpc } = await request.json();
    const stub = accountObjectStub(env.NANOCODEX_ACCOUNT_TOOLS, name);
    if (rpc) { try { return Response.json({ value: await stub[rpc]('${owner}') }); } catch (error) { return Response.json({ error: error.message }); } }
    const response = await stub.fetch('https://account-tools.internal' + path, { method: 'POST', headers: { 'x-nanocodex-owner-id': '${owner}', 'content-type': 'application/json' }, body: JSON.stringify(body) });
    return Response.json({ status: response.status, moved: response.headers.get('x-nanocodex-account-moved'), body: await response.text() });
  }
  if (url.pathname === '/__fixture/forget') return Response.json(await accountTools(env).getByName('${owner}').forgetMachine('${owner}', '${machine}', true));
  return accountTools(env).getByName('${owner}').fetch(request);
} };
`;

test("an account re-homes idle state, follows moves and keeps routes, receipts and revocation", { timeout: 90_000 }, async () => {
  const output = join(root, "../../output/account-placement-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, { recursive: true });
  const wire = [], runtime = [], sockets = [], observed = {};
  let mf, connector, failure;
  const waitFor = async (predicate, description, ms = 10_000) => {
    const deadline = performance.now() + ms;
    while (performance.now() < deadline) { const value = await predicate(); if (value) return value; await delay(20); }
    throw Error(`${description} exceeded ${ms}ms`);
  };
  let base;
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
  const outputOf = value => JSON.stringify(value);
  const home = region => `~home/v1/${region}/${owner}`;
  try {
    const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
      format: "esm", platform: "node", target: "es2022", conditions: ["workerd"],
      banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
      alias: { "node-rsa": root + "/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
      external: ["cloudflare:*", "node:*"], logLevel: "warning" });
    await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
    mf = new Miniflare({ modules: true, script: bundle.outputFiles[0].text,
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      durableObjects: { NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true } },
      handleRuntimeStdio(stdout, stderr) { for (const s of [stdout, stderr]) s.on("data", chunk => runtime.push(String(chunk))); } });
    base = await mf.ready;
    const native = await createNodeProcessTools({ workspace });
    const tools = await createTools({ tools: native.tools });
    const endpoint = new URL("/tool-host", base); endpoint.protocol = "ws:";
    connector = createAttachment(tools, { endpoint: endpoint.href, transport: { connect() {
      const attempt = sockets.length + 1;
      const socket = new WebSocket(endpoint, { headers: { authorization: credential, "x-nanocodex-owner-id": owner } });
      sockets.push(socket); wire.push({ attempt, event: "connect", at: Date.now() });
      socket.on("close", (code, reason) => wire.push({ attempt, event: "close", at: Date.now(), code, reason: String(reason) }));
      return socket;
    } } }, { machines: [{ id: machine, name: "Synthetic re-homed Hand", workspace, capabilities: ["shell"] }],
      attachmentId: machine, heartbeatMs: 100, reconnectDelayMs: 20, drainTimeoutMs: 1000 });
    await connector.connect();
    const initial = await waitFor(online, "legacy publication");
    const oldRoute = route(initial);
    assert.ok(oldRoute?.route_token);
    const first = await invoke("call-before", oldRoute.route_token, "printf A >> effects.log; printf BEFORE_OK");
    assert.match(outputOf(first), /BEFORE_OK/);
    observed.legacy_call = true;

    // Idle re-home: one export, sockets closed, home adopts and verifies.
    const moved = await api("/__fixture/rehome", { region: "wnam" });
    assert.equal(moved.moved, true, JSON.stringify(moved));
    assert.ok(wire.some(row => row.attempt === 1 && row.event === "close" && row.code === 1012), "legacy closed its host socket");
    const legacy = await api("/__fixture/raw", { name: owner, path: "/snapshot", body: { owner_id: owner } });
    assert.equal(legacy.status, 421);
    assert.equal(legacy.moved, home("wnam"));
    const legacyRpc = await api("/__fixture/raw", { name: owner, rpc: "listMachines" });
    assert.match(legacyRpc.error ?? "", new RegExp(`account_moved:${home("wnam").replace(/[/~]/g, "\\$&")}`));
    observed.legacy_tombstone_fails_closed = true;

    // The Hand reconnects through routing to the home; its route token and roots survive.
    const after = await waitFor(async () => sockets.length >= 2 ? online() : undefined, "home publication");
    assert.equal(route(after).route_token, oldRoute.route_token, "route identity copied with the account");
    assert.deepEqual(after.mount_roots, initial.mount_roots);
    assert.equal((await api("/__fixture/where")).at, home("wnam"));
    const replay = await invoke("call-before", oldRoute.route_token, "printf A >> effects.log; printf BEFORE_OK");
    assert.match(outputOf(replay), /BEFORE_OK/);
    const second = await invoke("call-after", oldRoute.route_token, "printf B >> effects.log; printf AFTER_OK");
    assert.match(outputOf(second), /AFTER_OK/);
    const { readFile } = await import("node:fs/promises");
    assert.equal(await readFile(join(workspace, "effects.log"), "utf8"), "AB", "receipt replay never re-executed");
    observed.home_serves_routes_and_receipts = true;

    // A pending call defers any move; nothing is exported while work is in flight.
    const slow = invoke("call-slow", oldRoute.route_token, "sleep 1; printf SLOW_OK");
    await delay(300);
    const deferred = await api("/__fixture/rehome", { region: "weur" });
    assert.equal(deferred.moved, false, JSON.stringify(deferred));
    assert.match(outputOf(await slow), /SLOW_OK/);
    observed.busy_account_never_moves = true;

    // Re-home path: wnam -> weur; a stale legacy hint walks two hops.
    const again = await api("/__fixture/rehome", { region: "weur" });
    assert.equal(again.moved, true, JSON.stringify(again));
    assert.equal((await api("/__fixture/raw", { name: home("wnam"), path: "/snapshot", body: { owner_id: owner } })).moved, home("weur"));
    assert.equal((await api("/__fixture/raw", { name: owner, path: "/snapshot", body: { owner_id: owner } })).moved, home("wnam"));
    await waitFor(async () => sockets.length >= 3 ? online() : undefined, "second home publication");
    assert.match(outputOf(await invoke("call-final", oldRoute.route_token, "printf FINAL_OK")), /FINAL_OK/);
    observed.rehome_chain = true;

    // Ambiguous adoption (verification unreachable) fails closed: the source stays
    // frozen (503 migrating, no successor published) until verification succeeds.
    await api("/__fixture/fail-verify", { fail: true });
    const ambiguous = await api("/__fixture/rehome", { region: "wnam" });
    assert.equal(ambiguous.moved, false, JSON.stringify(ambiguous));
    const frozen = await api("/__fixture/raw", { name: home("weur"), path: "/snapshot", body: { owner_id: owner } });
    assert.equal(frozen.status, 503);
    assert.equal(frozen.moved, null);
    const frozenRpc = await api("/__fixture/raw", { name: home("weur"), rpc: "listMachines" });
    assert.match(frozenRpc.error ?? "", /account_migrating/);
    // Idempotent adoption: the same migration re-applies nothing; the home never takes another export.
    await api("/__fixture/fail-verify", { fail: false });
    await waitFor(async () => (await api("/__fixture/raw", { name: home("weur"), path: "/snapshot", body: { owner_id: owner } })).moved === home("wnam"),
      "resumed verification publishes the successor", 15_000);
    assert.equal(runtime.join("").split("account.placement.import_failed").length - 1, 0);
    await waitFor(online, "Hand reconnects to the resumed home", 15_000);
    assert.match(outputOf(await invoke("call-resumed", oldRoute.route_token, "printf RESUMED_OK")), /RESUMED_OK/);
    observed.ambiguous_adoption_fails_closed = true;

    // Revocation still applies at the current home.
    await api("/__fixture/forget");
    const revoked = await snapshot();
    assert.ok(!revoked.machines.some(entry => entry.machine.id === machine && entry.online));
    observed.revocation_after_move = true;
    assert.ok(runtime.join("").includes("account.placement.adopted"));
  } catch (error) { failure = error; }
  finally {
    await connector?.close?.().catch(() => undefined);
    for (const socket of sockets) socket.terminate?.();
    await mf?.dispose().catch(() => undefined);
    await writeFile(join(output, "evidence.json"), JSON.stringify({ command, observed, wire, runtime: runtime.join("").slice(-20_000) }, null, 2));
  }
  if (failure) throw failure;
  console.log(JSON.stringify({ evidence: output, ...observed }));
});
