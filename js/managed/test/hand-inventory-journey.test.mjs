import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { Actions, connectActions, Transport } from "../../nanocodex/cloud/index.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "11111111-1111-4111-8111-111111111111";
const thread = "019b0000-0000-7000-8000-111111111111";
const principal = { kind: "api_key", userId: owner, organizationId: "22222222-2222-4222-8222-222222222222",
  teamId: "33333333-3333-4333-8333-333333333333", role: "owner", subjectId: `user:${owner}`,
  credentialId: "synthetic-inventory", authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"] };
const source = `
import worker, { AccountHostedTools, DurableAgentSession } from './src/index.ts';
import { routeManaged } from '../account/worker/managedProxy.ts';
export class InventoryAccount extends AccountHostedTools {
  async seedLegacy() {
    this.ctx.storage.sql.exec('CREATE TABLE workspace_hand_inventory (session_id TEXT PRIMARY KEY, entries_json TEXT, revision TEXT)');
    this.ctx.storage.sql.exec('INSERT INTO workspace_hand_inventory VALUES (?, ?, ?)', '${thread}',
      JSON.stringify([{id:'retired-thread-host',name:'Synthetic old workspace',kind:'workspace',online:null,health:'unknown'}]), 'old-publication');
    this.ctx.storage.kv.put('workspace_hand_inventory_overflow', true);
  }
}
export class InventorySession extends DurableAgentSession {
  async seed() {
    await (await super.fetch(new Request("https://fixture.internal/__initialize"))).body?.cancel();
    this.ctx.storage.sql.exec("INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://synthetic.example','managed',?)",
      '${thread}','${owner}','${principal.organizationId}','${principal.teamId}',Date.now());
  }
}
export default {async fetch(request,env,ctx) {
  const path=new URL(request.url).pathname;
  if(path==='/__fixture/legacy') { await env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').seedLegacy(); return new Response(null,{status:204}); }
  if(path==='/__fixture/session') { await env.NANOCODEX_SESSIONS.getByName('${thread}').seed(); return new Response(null,{status:204}); }
  // External account authentication is the only substituted public boundary.
  const auth=request.headers.get('authorization');
  if(!['inventory','reader','tools','none','connect','other'].some(role=>auth==='Bearer synthetic-'+role)) return Response.json({error:'unauthorized'},{status:401});
  const actor=${JSON.stringify(principal)};
  if(auth==='Bearer synthetic-reader') actor.capabilities=['agents:read'];
  if(auth==='Bearer synthetic-tools') actor.capabilities=['tools:use'];
  if(auth==='Bearer synthetic-none') actor.capabilities=[];
  if(auth==='Bearer synthetic-connect') actor.connectGrant={grantId:'synthetic-grant'};
  if(auth==='Bearer synthetic-other') actor.userId='44444444-4444-4444-8444-444444444444';
  return await routeManaged(request, { NANOCODEX_BACKEND: {
    fetch: forwarded => worker.fetch(new Request(forwarded, {
      cf: { continent: "EU", country: "DE", longitude: "8.68" },
    }),env,ctx,actor),
  } }, new URL(request.url)) ?? Response.json({error:'not_found'}, {status:404});
}};
`;

for (const regional of [false, true]) test(`account inventory and SDK retirement (${regional ? "versioned" : "legacy"})`, { timeout: 60_000 }, async () => {
  const output = join(repo, "output/hand-inventory-journey", `${Date.now()}-${process.pid}-${regional ? "regional" : "legacy"}`);
  await mkdir(output, { recursive: true });
  const http = [], wire = [], sockets = [], assets = [];
  let wasmSequence = 0;
  let mf, base, failure;
  const request = async (path, options = {}) => {
    const started = performance.now();
    const response = await fetch(new URL(path, base), { headers: { authorization: "Bearer synthetic-inventory" },
      ...options, signal: AbortSignal.timeout(10_000) });
    const text = await response.text();
    const value = text ? JSON.parse(text) : null;
    http.push({ path, method: options.method ?? "GET", actor: options.headers?.authorization ?? "default",
      status: response.status, cacheControl: response.headers.get("cache-control"),
      durationMs: Math.round(performance.now() - started), value });
    return { status: response.status, value };
  };
  async function publish(path, id, runtime = `${id}-runtime`) {
    const versioned = regional && path === "/v1/account/tool-host";
    const socket = new WebSocket(new URL(path, base).href.replace(/^http/, "ws"),
      { headers: { authorization: "Bearer synthetic-inventory", ...(versioned ? {
        "x-nanocodex-hand-machine-id": id, "x-nanocodex-hand-runtime-id": runtime,
      } : {}) } });
    sockets.push(socket);
    await new Promise((resolve, reject) => { socket.once("open", resolve); socket.once("error", reject); });
    const ready = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("catalog acknowledgement timed out")), 5_000);
      socket.once("close", (code, reason) => { clearTimeout(timer); reject(new Error(`catalog rejected: ${code} ${reason}`)); });
      socket.once("message", data => { clearTimeout(timer); const frame = JSON.parse(String(data));
        wire.push({ id, direction: "broker", frame }); resolve(frame); });
    });
    const catalog = { type: "catalog", attachment_id: id, ...(versioned ? { runtime_id: runtime } : {}), capabilities: ["turn_metadata"],
      machines: [{ id, name: id, workspace: "/synthetic/workspace", capabilities: ["native"] }],
      tools: [{ provider: "native", remote_name: "device_info", parallel_safe: true, timeout_ms: 15_000,
        definition: { type: "function", name: "device_info", description: "Synthetic device", strict: false,
          parameters: { type: "object", properties: {}, required: [], additionalProperties: false } } }] };
    wire.push({ id, direction: "publisher", frame: catalog }); socket.send(JSON.stringify(catalog));
    assert.equal((await ready).type, "ready");
    return socket;
  }
  const inventory = () => request("/v1/account/hands/inventory");
  const disconnect = socket => new Promise(resolve => { socket.once("close", resolve); socket.terminate(); });
  const start = async bundle => {
    mf = new Miniflare({ port: 0, durableObjectsPersist: join(output, "sqlite"),
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle }, ...assets],
      durableObjects: { NANOCODEX_ACCOUNT_TOOLS: { className: "InventoryAccount", useSQLite: true },
        NANOCODEX_SESSIONS: { className: "InventorySession", useSQLite: true } },
            r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"],
      serviceBindings: { NANOCODEX: async request => {
        const path = new URL(request.url).pathname;
        if (path.startsWith("/subjects/")) return new Response(null, { status: 204 });
        if (path.endsWith("/catalog")) return Response.json({ connectors: {}, mcp_connections: [] });
        if (path.endsWith("/credentials/vault")) return Response.json({ vault: [] });
        return Response.json({ tools: [], machines: [], connections: [] });
      } } });
    base = await mf.ready;
  };
  try {
    const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
      metafile: true, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
      external: ["cloudflare:*", "node:*"],
      alias: { "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"),
        "node-rsa": join(repo, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
      plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
        const path = join(args.resolveDir, args.path), name = `fixture-${wasmSequence++}.wasm`;
        assert.ok(path.startsWith(repo));
        assets.push({ type: "CompiledWasm", path: name, contents: await readFile(path) });
        return { path: `./${name}`, external: true };
      }); } }], logLevel: "silent" });
    const code = bundle.outputFiles[0].text;
    await writeFile(join(output, "worker.mjs"), code);
    await writeFile(join(output, "source-resolution.json"), JSON.stringify(Object.keys(bundle.metafile.inputs), null, 2));
    await start(code);
    assert.equal((await request("/__fixture/legacy", { method: "POST" })).status, 204);
    await mf.dispose();
    await start(code);
    assert.deepEqual(await inventory(), { status: 200,
      value: { data: [], coverage: "known_account_and_workspace", complete: true } });
    assert.equal((await request("/v1/account/hands/inventory", { headers: {} })).status, 401);
    for (const actor of ["reader", "tools", "none", "connect"]) {
      assert.equal((await request("/v1/account/hands/inventory",
        { headers: { authorization: `Bearer synthetic-${actor}` } })).status, 403);
    }
    assert.equal((await request("/v1/account/hands/inventory", { method: "POST" })).status, 405);
    assert.equal((await request("/v1/account/hands/inventory?owner=other")).status, 400);
    assert.equal((await request("/__fixture/session", { method: "POST" })).status, 204);
    const transport = Transport.http(base, { fetch: async (url, init) => {
      const headers = new Headers(init.headers);
      headers.set("authorization", "Bearer synthetic-inventory");
      const response = await fetch(url, { ...init, headers, signal: AbortSignal.timeout(10_000) });
      http.push({ sdk: true, path: new URL(url).pathname, method: init.method,
        status: response.status, value: await response.clone().json() });
      return response;
    } }).setup({ appId: "synthetic-hand-inventory" });
    const sdk = connectActions()(transport);
    assert.deepEqual(await sdk.hand.list(), (await inventory()).value);
    assert.throws(() => sdk.hand.forget("../other"), TypeError);
    assert.throws(() => sdk.hand.forget("account-device", { force: "true" }), TypeError);
    const accountSocket = await publish("/v1/account/tool-host", "account-device");
    // Thread-scoped workspace Hands are retired: a native catalog on the
    // thread tool host fails with a migration error and never joins inventory.
    await assert.rejects(publish(`/v1/agents/${thread}/tool-host`, "thread-device"), /hand_migration_required/);
    const expected = { status: 200, value: { data: [{ id: "account-device", name: "account-device",
      kind: "hand", online: true, health: "connected" }], coverage: "known_account_and_workspace", complete: true } };
    assert.deepEqual(await sdk.hand.list(), expected.value);
    await assert.rejects(sdk.hand.forget("account-device"), { status: 409 });
    if (regional) {
      const relays = await request("/v1/account/hand-relays");
      assert.deepEqual(relays.value.regional, [], "every publication lives on the account object");
    }
    assert.deepEqual(await inventory(), expected);
    assert.equal(http.at(-1).cacheControl, "no-store");
    assert.deepEqual(await request('/v1/account/hands/account-device', {method:'DELETE'}),
      {status:409,value:{error:'hand_online'}});
    assert.deepEqual(await request('/v1/account/hands/prune', {method:'POST'}),
      {status:200,value:{forgotten:[],complete:true}});
    for (const actor of ['reader', 'tools', 'none', 'connect']) {
      for (const [path,method] of [['/v1/account/hands/account-device','DELETE'],['/v1/account/hands/prune','POST']]) {
        assert.equal((await request(path,{method,headers:{authorization:`Bearer synthetic-${actor}`}})).status,403);
      }
    }
    assert.deepEqual(await request('/v1/account/hands/account-device?force=1',
      {method:'DELETE',headers:{authorization:'Bearer synthetic-other'}}),{status:200,value:{forgotten:false}});
    assert.equal((await request('/v1/account/hands/%ZZ',{method:'DELETE'})).status,404);
    assert.deepEqual(await request("/v1/account/hands/inventory",
      { headers: { authorization: "Bearer synthetic-other" } }), { status: 200,
      value: { data: [], coverage: "known_account_and_workspace", complete: true } });
    await assert.rejects(publish(`/v1/agents/${thread}/tool-host`, "thread-device"), /hand_migration_required/);
    assert.deepEqual(await inventory(), expected);
    await disconnect(accountSocket);
    const offline = { status: 200, value: {
      data: [{ id: "account-device", name: "account-device", kind: "hand", online: false, health: "offline" }],
      coverage: "known_account_and_workspace", complete: true } };
    const deadline = Date.now() + 5_000;
    let observed;
    do {
      observed = await inventory();
      if (observed.value.data[0]?.online === false) break;
      await new Promise(resolve => setTimeout(resolve, 25));
    } while (Date.now() < deadline);
    assert.deepEqual(observed, offline);
    assert.deepEqual(await sdk.hand.prune(), {forgotten:['account-device'],complete:true});
    assert.deepEqual((await inventory()).value.data,[]);
    assert.deepEqual(await request('/v1/account/hands/account-device',{method:'DELETE'}),
      {status:200,value:{forgotten:false}});
    const forced = await publish('/v1/account/tool-host','vm:forced-device');
    const forcedClosed = new Promise(resolve => forced.once('close',resolve));
    assert.deepEqual(await sdk.hand.forget('vm:forced-device', {force:true}), {forgotten:true});
    await Promise.race([forcedClosed,new Promise((_,reject)=>setTimeout(()=>reject(new Error('forgotten Hand remained connected')),3000))]);
    assert.deepEqual((await inventory()).value.data,[]);
    await mf.dispose();
    await start(code);
    assert.deepEqual((await inventory()).value.data,[]);
    if (regional) {
      // Tombstones survive restart and prevent forgotten runtimes reappearing.
      await assert.rejects(publish('/v1/account/tool-host', 'vm:forced-device'), /409/);
      await assert.rejects(publish('/v1/account/tool-host', 'account-device'), /409/);
      const fresh = await publish('/v1/account/tool-host', 'vm:forced-device', 'fresh-runtime');
      assert.equal((await inventory()).value.data[0].online, true);
      await disconnect(fresh);
    }
    // The standalone public action uses the same real HTTP transport.
    const finalTransport = Transport.http(base, { fetch: (url, init) => fetch(url, {
      ...init, headers: { ...Object.fromEntries(new Headers(init.headers)), authorization: "Bearer synthetic-inventory" },
    }) }).setup({ appId: "synthetic-hand-inventory" });
    assert.equal((await Actions.hand.list(finalTransport)).complete, true);
  } catch (error) { failure = error; throw error; }
  finally {
    for (const socket of sockets) socket.terminate();
    await mf?.dispose();
    await writeFile(join(output, "evidence.json"), JSON.stringify({
      command: "pnpm --filter nanocodex-managed-service run test:hand-inventory", http, wire,
      expected: "only account Hands; SDK list/forget/prune over HTTP; online protection; offline prune; forced socket closure; regional tombstones survive restart and permit a fresh runtime", regional,
      passed: !failure, error: failure?.stack }, null, 2));
    console.log(`Hand inventory evidence: ${output}`);
  }
});
