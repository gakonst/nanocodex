import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000081";
const other = "00000000-0000-4000-8000-000000000082";
const command = "pnpm --filter nanocodex-managed-service test:regional-hand-relay";

// Enrollment and ingress geography are synthetic. Authentication, proxy,
// managed route, provider, directory, relays, publisher and /bin/sh are real.
// Observed subclasses log requests and can delay a real admission reply; the
// fixture driver retains the provider/process handles that an agent would use.
const source = `

import { DurableObject } from 'cloudflare:workers';
import managed from './src/index.ts';
export * from './src/index.ts';
import { ensureAccount, createApiKey, authenticate } from './src/account-auth.ts';
import { AccountHostedTools, AccountHostedToolsCallRoutes, AccountHostedToolsProvider } from './src/account-hosted-tools.ts';
import { RegionalHandRelay } from './src/regional-hand-relay.ts';
import { routeManaged } from '../account/worker/managedProxy.ts';
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class ObservedAccountHostedTools extends AccountHostedTools {
  async fetch(request) {
    const path=new URL(request.url).pathname;
    if(path==='/__fixture/legacy-era') {
      // Reconstruct the upgrade-era state: a real retained broker catalog
      // written before the regional directory existed. This is fixture setup.
      const {machine}=await request.json();
      this.ctx.storage.sql.exec('DELETE FROM regional_hand_directory WHERE machine_id=?',machine);
      return Response.json({prepared:true});
    }
    if(path==='/__fixture/admission/hold') { this.holdNext=true; return Response.json({held:false}); }
    if(path==='/__fixture/admission/status') return Response.json({held:this.held===true});
    if(path==='/__fixture/admission/release') { this.release?.(); return Response.json({released:true}); }
    console.info({type:'fixture.route',target:'directory',path});
    const hold=path==='/regional/claim'&&this.holdNext; if(hold)this.holdNext=false;
    const response=await super.fetch(request);
    // Delay only delivery of the real directory's successful admission reply.
    // Another real publication can revoke it before its broker sends ready.
    if(hold&&response.ok) { this.held=true; await new Promise(resolve=>{this.release=resolve;}); this.held=false; this.release=undefined; }
    return response;
  }
}
export class ObservedRegionalHandRelay extends RegionalHandRelay {
  async fetch(request) { console.info({type:'fixture.route',target:'relay',path:new URL(request.url).pathname}); return super.fetch(request); }
}
export class FixtureDriver extends DurableObject {
  constructor(ctx,env) { super(ctx,env); this.providers=new Map(); this.handles=new Map(); }
  async fetch(request) {
    const {owner,operation,id,machine,name='exec_command',input,call}=await request.json();
    if(operation==='legacy-era')return this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner).fetch('https://account-tools.internal/__fixture/legacy-era',{method:'POST',body:JSON.stringify({machine})});
    if(operation==='admission')return this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner).fetch('https://account-tools.internal/__fixture/admission/'+input);
    let provider=this.providers.get(owner);
    if(!provider) { provider=new AccountHostedToolsProvider(this.env.NANOCODEX_ACCOUNT_TOOLS,owner,()=>true,'00000000-0000-7000-8000-000000000083',this.env.NANOCODEX_HAND_RELAYS,new AccountHostedToolsCallRoutes(this.ctx.storage)); this.providers.set(owner,provider); }
    if(operation==='refresh') { await provider.refresh(); return Response.json({machines:provider.machines(),definitions:provider.definitions()}); }
    if(operation==='malformed-bind') {
      // Fault only the discovery token returned by the real owner. The actual
      // provider must reject this reserved envelope before any legacy RPC.
      const namespace={getByName:(owner)=>({fetch:async(...args)=>{
        const response=await this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner).fetch(...args);
        const snapshot=await response.json();
        for(const entry of snapshot.machines)for(const tool of entry.tools)tool.route_token=input;
        return Response.json(snapshot);
      }})};
      const malformed=new AccountHostedToolsProvider(namespace,owner,()=>true,undefined,this.env.NANOCODEX_HAND_RELAYS);
      await malformed.refresh(); this.handles.set(id,malformed.machineTool(machine,name)); return Response.json({bound:true});
    }
    if(operation==='bind') { await provider.refresh(); const tool=provider.machineTool(machine,name); if(!tool)return new Response(null,{status:404}); this.handles.set(id,tool); return Response.json({route:tool.routeToken}); }
    if(operation==='invoke') {
      const tool=this.handles.get(id); if(!tool)return new Response(null,{status:404});
      const value=await tool.handler(input,{sessionId:'regional-relay-session',callId:call,model:'synthetic-model'});
      const process=value?.[Symbol.for('nanocodex.processSessionTool')];
      if(process)this.handles.set(id+':process',process);
      return Response.json(value);
    }
    return new Response(null,{status:404});
  }
}
export default { async fetch(request,env,ctx) {
  const url=new URL(request.url);
  if(env.EDGE) {
    if(url.pathname.startsWith('/__fixture')) return env.NANOCODEX_BACKEND.fetch(request);
    return await routeManaged(request,env,url) ?? new Response(null,{status:404});
  }
  if(url.pathname==='/__fixture/issue') {
    const body=await request.json(); await ensureAccount(env,body.owner,true);
    const auth=await (await env.NANOCODEX_USERS.getByName(body.owner).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env,{kind:'api_key',userId:body.owner,...auth.grant,
      subjectId:'api_key:'+body.owner,credentialId:'fixture',capabilities:body.capabilities??auth.grant.capabilities},'Synthetic regional relay'));
  }
  if(url.pathname==='/__fixture/driver') {
    const principal=await authenticate(request,env,url); if(!principal)return new Response(null,{status:401});
    const body=await request.json();
    return env.DRIVER.getByName(principal.userId).fetch('https://driver.internal/',{method:'POST',body:JSON.stringify({...body,owner:principal.userId})});
  }
  // Real clients cannot assign Request.cf. This fixture simulates workerd's
  // trusted ingress metadata, while ordinary spoofable headers remain intact.
  const colo=request.headers.get('x-fixture-colo')??'FRA';
  const cloned=new Request(request,{cf:colo==='SJC'?{colo,continent:'NA',country:'US',longitude:'-121.89'}:{colo,continent:'EU',country:'DE',longitude:'8.68'}});
  return managed.fetch(cloned,env,ctx);
}};
`;

test("public regional Hand relays preserve discovery, process ownership and uncertain outcomes", { timeout: 120_000 }, async () => {
  const output = join(root, "output/regional-hand-relay", `${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const wire = [], http = [], runtime = [], publishers = [];
  const evidence = { command, inputs: { owner, other, ingress: ["FRA", "SJC"], shell: "/bin/sh" },
    expected: { public_auth: true, aggregate_legacy_regional: true, direct_provider: true, same_runtime_reconnect: true, cross_region_replacement: true,
      stable_paths: true, collision_rejected: true, persistent_directory: true, persistent_call_route: true, cached_process_pinned: true, region_return: true, unrelated_retirement_safe: true, revoked_admission_never_ready: true, public_inventory_retirement: true, pending_retirement_rejected: true, retained_legacy_collision: true, machineless_collision: true, malformed_reserved_routes: true, uncertain_dispatches: 1, replacement_dispatches: 1 }, observed: {} };
  let mf, base, token, failure, announcedRuntimeId;
  const originalInfo = console.info;
  console.info = (record, ...rest) => {
    if (record?.type === "hand.attachment") {
      runtime.push(JSON.stringify(record));
      if (record.event === "connection_start") announcedRuntimeId = record.runtime_id;
    } else originalInfo(record, ...rest);
  };
  const request = async (path, body, key = token) => {
    const response = await fetch(new URL(path, base), { method: body === undefined ? "GET" : "POST",
      headers: { ...(key ? { authorization: `Bearer ${key}` } : {}), "content-type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(20_000) });
    const text = await response.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
    http.push({ path, status: response.status, ...(path.endsWith("/issue") ? {} : { input: body, value }) });
    return { status: response.status, value };
  };
  const ok = async promise => { const r = await promise; assert.equal(r.status, 200, JSON.stringify(r)); return r.value; };
  const driver = body => ok(request("/__fixture/driver", body));
  const discover = key => ok(request("/v1/account/hands", undefined, key));
  const bind = (id, machine) => driver({ operation: "bind", id, machine });
  const invoke = (id, call, input) => driver({ operation: "invoke", id, call, input });
  const shell = (cmd, yield_time_ms = 1000) => ({ cmd, shell: "/bin/sh", login: false, yield_time_ms });
  const waitFor = async (predicate, label) => {
    const deadline = performance.now() + 10_000;
    while (performance.now() < deadline) { const value = await predicate(); if (value) return value; await delay(20); }
    assert.fail(`${label}; evidence: ${output}`);
  };
  const calls = label => wire.filter(r => r.direction === "broker" && r.frame?.type === "call" && (!label || r.label === label));
  const publish = async ({ label, id, name = "Shared Hand", colo = "FRA", extra = {}, reconnect = true, regional = true, machineless = false }) => {
    const workspace = join(output, label); await mkdir(workspace, { recursive: true });
    const native = await createNodeProcessTools({ workspace });
    const tools = await createTools({ tools: { ...(machineless ? {} : Object.fromEntries(native.tools.map(t => [t.name, t]))), ...extra } });
    const peer = { label, id, workspace, native, tools, colo, sockets: [] }; publishers.push(peer);
    const endpoint = new URL("/v1/account/tool-host", base).href.replace(/^http/, "ws");
    peer.attachment = createAttachment(tools, { endpoint, transport: { async connect() {
      // Native publishers know this identity before opening their socket. The
      // JS fixture obtains its identical ID from the publisher's observation;
      // catalog frames and command execution are never rewritten.
      peer.runtimeId ??= announcedRuntimeId;
      if (peer.gate) await peer.gate;
      const socket = new WebSocket(endpoint, { headers: { authorization: `Bearer ${token}`, "x-fixture-colo": peer.colo,
        ...(regional ? { "x-nanocodex-hand-machine-id": id, "x-nanocodex-hand-runtime-id": peer.runtimeId } : {}),
        "x-nanocodex-owner-id": other, "x-nanocodex-hand-relay-region": "apac" } });
      peer.sockets.push(socket); peer.socket = socket;
      socket.on("upgrade", response => wire.push({ label, event: "upgrade", status: response.statusCode, timing: response.headers["server-timing"] }));
      const send = socket.send.bind(socket);
      socket.send = (data, ...args) => {
        const frame = JSON.parse(String(data));
        if (frame.type === "catalog") assert.equal(frame.runtime_id, peer.runtimeId);
        wire.push({ label, direction: "host", frame }); return send(data, ...args);
      };
      const emit = socket.emit.bind(socket);
      socket.emit = (event, ...args) => {
        if (event === "message") {
          const frame = JSON.parse(String(args[0])); wire.push({ label, direction: "broker", frame });
          if (peer.dropNextCall && frame.type === "call") { peer.dropNextCall = false; socket.terminate(); return false; }
        }
        return emit(event, ...args);
      };
      socket.on("close", (code, reason) => wire.push({ label, event: "close", code, reason: String(reason) }));
      return socket;
    } } }, { attachmentId: id, machines: machineless ? [] : [{ id, name, workspace, capabilities: ["shell"] }],
      heartbeatMs: 250, reconnectDelayMs: 20, drainTimeoutMs: 500, reconnect });
    peer.client = await peer.attachment.connect(); return peer;
  };
  try {
    const assets = [];
    const bundle = await build({ stdin: { contents: source, resolveDir: join(root, "js/managed") }, bundle: true, write: false, metafile: true,
      format: "esm", platform: "node", conditions: ["workerd"], target: "es2022", external: ["cloudflare:*", "node:*"],
      banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
      alias: { "node-rsa": join(root, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
      plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
        const contents = await readFile(join(args.resolveDir, args.path));
        const name = `fixture-${assets.length}.wasm`;
        assets.push({ type: "CompiledWasm", path: name, contents });
        return { path: `./${name}`, external: true };
      }); } }], logLevel: "silent" });
    const resolutions = Object.fromEntries(["nanocodex/tools", "nanocodex-tools/attachment", "nanocodex-tools/node"].map(n => [n, fileURLToPath(import.meta.resolve(n))]));
    for (const path of Object.values(resolutions)) assert.ok(path.startsWith(root), `dependency escaped checkout: ${path}`);
    await writeFile(join(output, "source-resolution.json"), JSON.stringify({ resolutions, bundleInputs: Object.keys(bundle.metafile.inputs) }, null, 2));
    await writeFile(join(output, "fixture-source.mjs"), source);
    await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
    const worker = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets] };
    mf = new Miniflare({ port: 0, durableObjectsPersist: join(output, "sqlite"),
      handleRuntimeStdio(stdout, stderr) { for (const stream of [stdout, stderr]) createInterface({ input: stream }).on("line", line => runtime.push(line)); },
      workers: [{ ...worker, name: "edge", bindings: { EDGE: true }, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
        { ...worker, name: "managed", bindings: { NANOCODEX_REGIONAL_HAND_RELAYS: "true" }, durableObjects: Object.fromEntries([
          ["NANOCODEX_USERS", "UserAccount"], ["NANOCODEX_ORGANIZATIONS", "Organization"], ["NANOCODEX_API_KEYS", "ApiKeyRecord"],
          ["NANOCODEX_AUTH", "NonceStorage"], ["NANOCODEX_ACCOUNT_TOOLS", "ObservedAccountHostedTools"],
          ["NANOCODEX_HAND_RELAYS", "ObservedRegionalHandRelay"], ["DRIVER", "FixtureDriver"],
        ].map(([binding, className]) => [binding, { className, useSQLite: true }])) }] });
    base = await mf.ready;
    token = (await ok(request("/__fixture/issue", { owner }))).token;
    const otherToken = (await ok(request("/__fixture/issue", { owner: other }))).token;
    const restricted = (await ok(request("/__fixture/issue", { owner, capabilities: ["agents:read"] }))).token;
    assert.equal((await request("/v1/account/hands", undefined, "ncx_live_invalid")).status, 401);
    assert.equal((await request("/v1/account/hands", undefined, restricted)).status, 403);
    assert.equal((await request("/v1/account/tool-host")).status, 426);
    const deniedUpgrade = async (key, expected, headers = {}) => {
      const socket = new WebSocket(new URL("/v1/account/tool-host", base).href.replace(/^http/, "ws"), { headers: { authorization: `Bearer ${key}`, ...headers } });
      const status = await new Promise((resolve, reject) => {
        socket.once("unexpected-response", (_, response) => { response.resume(); resolve(response.statusCode); });
        socket.once("open", () => reject(Error("unauthorized upgrade admitted"))); socket.once("error", reject);
      });
      socket.terminate(); http.push({ path: "/v1/account/tool-host", upgrade: true, status }); assert.equal(status, expected);
    };
    await deniedUpgrade("ncx_live_invalid", 401); await deniedUpgrade(restricted, 403);
    evidence.observed.public_auth = true;
    const echo = { regional_echo: { description: "Synthetic regional echo", parameters: { type: "object", properties: {}, additionalProperties: false }, handler: () => "ECHO" } };
    const first = await publish({ label: "first", id: "regional-hand-a", extra: echo });
    const second = await publish({ label: "second", id: "regional-hand-b", colo: "SJC" });
    const legacy = await publish({ label: "legacy", id: "legacy-hand", extra: { legacy_echo: echo.regional_echo }, regional: false, reconnect: false });
    await driver({ operation: "legacy-era", machine: legacy.id });
    const initial = await discover(); assert.equal(initial.data.length, 3);
    const paths = Object.fromEntries(initial.data.map(m => [m.id, m.workspace]));
    assert.equal(new Set(Object.values(paths)).size, 3);
    assert.ok(Object.values(paths).every(path => path.startsWith("/shared-hand")));
    assert.deepEqual((await discover(otherToken)).data, []);
    // Different machine/tool pairs collide on the exact exposed public name.
    await assert.rejects(publish({ label: "collision", id: "regional-hand-a_regional", colo: "SJC", extra: { echo: echo.regional_echo }, reconnect: false }), /catalog_contract_mismatch|collision/);
    await assert.rejects(publish({ label: "regional-legacy-collision", id: "legacy-hand_legacy", colo: "SJC", extra: { echo: echo.regional_echo }, reconnect: false }), /catalog_contract_mismatch|collision/);
    await assert.rejects(publish({ label: "legacy-regional-collision", id: "regional-hand-a_regional", regional: false, extra: { echo: echo.regional_echo }, reconnect: false }), /catalog_contract_mismatch|collision/);
    assert.equal((await discover()).data.length, 3);
    for (const label of ["collision", "regional-legacy-collision", "legacy-regional-collision"]) assert.equal(calls(label).length, 0);
    evidence.observed.collision_rejected = true;
    const plain = await publish({ label: "machineless-first", id: "plain-first", machineless: true, regional: false,
      extra: { user_plain_hand_echo: echo.regional_echo }, reconnect: false });
    await assert.rejects(publish({ label: "machineless-regional-collision", id: "plain_hand", extra: { echo: echo.regional_echo }, reconnect: false }), /catalog_contract_mismatch|collision/);
    await assert.rejects(publish({ label: "regional-machineless-collision", id: "plain-second", machineless: true, regional: false,
      extra: { "user_regional-hand-a_regional_echo": echo.regional_echo }, reconnect: false }), /catalog_contract_mismatch|collision/);
    await plain.attachment.close();
    evidence.observed.machineless_collision = true;
    assert.doesNotMatch((await bind("legacy", legacy.id)).route, /^hand-relay:/);
    evidence.observed.aggregate_legacy_regional = true;
    const invocationsBeforeMalformed = () => runtime.filter(line => line.includes('"path":"/invoke"')).length;
    for (const malformed of ["hand-relay:v2:weur:bad", "hand-relay:v1:unknown:bad", "hand-relay:v1:weur:"]) {
      await driver({ operation: "malformed-bind", id: "malformed", machine: first.id, input: malformed });
      const before = invocationsBeforeMalformed();
      assert.equal((await invoke("malformed", "malformed-" + malformed, shell("printf MUST_NOT_RUN"))).structuredResult.status, "unavailable");
      assert.equal(invocationsBeforeMalformed(), before, "malformed reserved tokens cannot fall back to the legacy broker");
    }
    evidence.observed.malformed_reserved_routes = true;
    const route = await bind("exec", first.id); assert.match(route.route, /^hand-relay:v1:weur:/);
    assert.match((await bind("second", second.id)).route, /^hand-relay:v1:wnam:/);
    assert.equal((await invoke("exec", "first-exec", shell("printf E >> effects.log; printf REGIONAL_OK"))).structuredResult.output, "REGIONAL_OK");
    const running = await invoke("exec", "process-start", shell("printf START; while [ ! -f release-process ]; do sleep 0.02; done; printf FINISH"));
    assert.ok(Number.isInteger(running.structuredResult.session_id));
    let releaseReconnect;
    first.gate = new Promise(resolve => { releaseReconnect = resolve; }); first.socket.terminate();
    await waitFor(() => !first.client.connected, "socket detached");
    // A changed ingress must retain the living runtime's original relay.
    first.colo = "SJC"; first.gate = undefined; releaseReconnect();
    await waitFor(() => first.client.connected && first.sockets.length >= 2, "same runtime reconnect");
    assert.equal((await bind("fresh", first.id)).route, route.route);
    await writeFile(join(first.workspace, "release-process"), "release");
    const polled = await invoke("exec:process", "process-poll", { session_id: running.structuredResult.session_id, chars: "", yield_time_ms: 1000 });
    assert.equal(polled.structuredResult.output, "FINISH"); assert.equal(polled.structuredResult.exit_code, 0);
    evidence.observed.same_runtime_reconnect = true;
    first.dropNextCall = true;
    const missingInput = shell("printf MUST_NOT_EXIST >> missing.log");
    assert.equal((await invoke("fresh", "uncertain-once", missingInput)).structuredResult.status, "ambiguous");
    const missingFrame = calls("first").find(r => r.frame.input.cmd === missingInput.cmd); assert.ok(missingFrame);
    assert.equal((await invoke("fresh", "uncertain-once", missingInput)).structuredResult.status, "ambiguous");
    assert.equal(calls().filter(r => r.frame.call_id === missingFrame.frame.call_id).length, 1);
    await assert.rejects(readFile(join(first.workspace, "missing.log")), { code: "ENOENT" });
    evidence.observed.uncertain_dispatches = 1;
    const replacedInput = shell("printf X >> replaced.log; while [ ! -f release-replaced ]; do sleep 0.02; done; printf OLD_RESULT", 30000);
    const pending = invoke("fresh", "replaced-inflight", replacedInput); pending.catch(() => {});
    await waitFor(async () => { try { return await readFile(join(first.workspace, "replaced.log"), "utf8") === "X"; } catch (error) { if (error.code !== "ENOENT") throw error; } }, "native effect before replacement");
    const replacement = await publish({ label: "replacement", id: first.id, name: "Renamed Hand", colo: "SJC" });
    assert.equal((await pending).structuredResult.status, "ambiguous");
    await writeFile(join(first.workspace, "release-replaced"), "release");
    assert.equal((await invoke("fresh", "replaced-inflight", replacedInput)).structuredResult.status, "ambiguous");
    assert.equal(calls().filter(r => r.frame.input.cmd === replacedInput.cmd).length, 1);
    await assert.rejects(readFile(join(replacement.workspace, "replaced.log")), { code: "ENOENT" });
    evidence.observed.replacement_dispatches = 1;
    await first.attachment.close();
    assert.ok(wire.some(r => r.label === "first" && r.event === "close" && r.code === 1008));
    const afterReplacement = await discover();
    assert.deepEqual(Object.fromEntries(afterReplacement.data.map(m => [m.id, m.workspace])), paths);
    const replacementRoute = await bind("replacement", first.id); assert.match(replacementRoute.route, /^hand-relay:v1:wnam:/);
    assert.notEqual(replacementRoute.route, route.route);
    assert.equal((await invoke("replacement", "replaced-inflight", replacedInput)).structuredResult.status, "ambiguous", "fresh binding must preserve admitted call routing across regions");
    assert.equal((await invoke("exec:process", "old-process", { session_id: running.structuredResult.session_id, chars: "MUST_NOT_SEND", yield_time_ms: 1000 })).structuredResult.status, "ambiguous", "retired routes retain uncertainty and must never resend stdin");
    assert.equal(calls("replacement").length, 0);
    assert.equal((await invoke("replacement", "fresh-after-replacement", shell("printf R >> effects.log; printf REPLACED_OK"))).structuredResult.output, "REPLACED_OK");
    assert.equal(await readFile(join(replacement.workspace, "effects.log"), "utf8"), "R");
    evidence.observed.cross_region_replacement = true; evidence.observed.stable_paths = true;
    const routeEvents = runtime.flatMap(line => { const start = line.indexOf('{"type":"fixture.route"'); if (start < 0) return []; try { return [JSON.parse(line.slice(start))]; } catch { return []; } });
    assert.ok(routeEvents.some(e => e.target === "relay" && e.path === "/invoke"));
    assert.equal(routeEvents.filter(e => e.target === "directory" && e.path === "/invoke").length, 0);
    evidence.observed.direct_provider = true;
    await mf.unsafeEvictDurableObject("managed", "ObservedAccountHostedTools", { name: owner, webSockets: "hibernate" });
    assert.deepEqual((await discover()).data, afterReplacement.data);
    assert.equal((await invoke("replacement", "after-directory-restart", shell("printf PERSISTED_OK"))).structuredResult.output, "PERSISTED_OK");
    evidence.observed.persistent_directory = true;
    await mf.unsafeEvictDurableObject("managed", "FixtureDriver", { name: owner });
    await bind("replacement", first.id);
    const beforeReplay = calls("replacement").length;
    assert.equal((await invoke("replacement", "replaced-inflight", replacedInput)).structuredResult.status, "ambiguous", "caller restart must preserve the original regional effect route");
    assert.equal(calls("replacement").length, beforeReplay);
    evidence.observed.persistent_call_route = true;
    // A cached exec receipt must retain its original runtime's process route,
    // even when a fresh provider handle replays it after same-region replacement.
    const cachedInput = shell("printf CACHE_START; while [ ! -f release-cached ]; do sleep 0.02; done; printf CACHE_FINISH");
    const cached = await invoke("replacement", "cached-process", cachedInput);
    assert.ok(Number.isInteger(cached.structuredResult.session_id));
    const sameRegion = await publish({ label: "same-region", id: first.id, name: "Renamed Again", colo: "SJC" });
    await replacement.attachment.close();
    await bind("same-region", first.id);
    const replayed = await invoke("same-region", "cached-process", cachedInput);
    assert.equal(replayed.structuredResult.status, "ambiguous", "cached process receipt must not become a replacement runtime process handle");
    assert.equal(calls("same-region").length, 0, "cached process must never execute or poll the replacement runtime");
    await writeFile(join(replacement.workspace, "release-cached"), "release");
    evidence.observed.cached_process_pinned = true;
    const returned = await publish({ label: "returned", id: first.id, name: "Shared Hand", colo: "FRA" });
    await sameRegion.attachment.close();
    assert.match((await bind("returned", first.id)).route, /^hand-relay:v1:weur:/);
    assert.equal((await invoke("returned", "returned-exec", shell("printf RETURNED_OK"))).structuredResult.output, "RETURNED_OK");
    evidence.observed.region_return = true;

    await driver({ operation: "admission", input: "hold" });
    const unrelatedPending = publish({ label: "held-unrelated", id: first.id, colo: "SJC", reconnect: false });
    unrelatedPending.catch(() => {});
    await waitFor(async () => (await driver({ operation: "admission", input: "status" })).held, "admission reply held");
    const unrelated = await publish({ label: "unrelated", id: second.id, colo: "FRA" });
    await second.attachment.close();
    await driver({ operation: "admission", input: "release" });
    const admitted = await unrelatedPending;
    assert.equal(admitted.client.connected, true, "retiring an unrelated route must not reject pending admission");
    await returned.attachment.close();
    evidence.observed.unrelated_retirement_safe = true;

    await driver({ operation: "admission", input: "hold" });
    const revokedPending = publish({ label: "held-revoked", id: first.id, colo: "FRA", reconnect: false });
    revokedPending.catch(() => {});
    await waitFor(async () => (await driver({ operation: "admission", input: "status" })).held, "revocable admission reply held");
    const winner = await publish({ label: "winner", id: first.id, colo: "SJC" });
    await driver({ operation: "admission", input: "release" });
    await assert.rejects(revokedPending, /route_revoked|catalog_contract_mismatch|policy|stale/);
    assert.equal(wire.filter(r => r.label === "held-revoked" && r.direction === "broker" && r.frame?.type === "ready").length, 0);
    assert.equal(calls("held-revoked").length, 0);
    await admitted.attachment.close();
    assert.match((await bind("winner", first.id)).route, /^hand-relay:v1:wnam:/);
    assert.equal((await invoke("winner", "winning-exec", shell("printf WINNER_OK"))).structuredResult.output, "WINNER_OK");
    evidence.observed.revoked_admission_never_ready = true;

    // Operational migration uses the shipped owner inventory and explicit
    // retirement endpoint; an offline legacy socket alone permits no takeover.
    const inventoryPath = "/v1/account/hand-relays", retirePath = inventoryPath + "/retire";
    assert.equal((await request(inventoryPath, undefined, "ncx_live_invalid")).status, 401);
    assert.equal((await request(inventoryPath, undefined, restricted)).status, 403);
    assert.deepEqual((await ok(request(inventoryPath, undefined, otherToken))).legacy, []);
    const pendingLegacy = await publish({ label: "pending-legacy", id: "pending-legacy-hand", regional: false });
    await bind("pending-legacy", pendingLegacy.id);
    const pendingLegacyCall = invoke("pending-legacy", "pending-legacy-call", shell("printf PENDING > pending.log; while [ ! -f release-process ]; do sleep 0.02; done; printf RECOVERED_OK", 30000));
    pendingLegacyCall.catch(() => {});
    await waitFor(async () => { try { return await readFile(join(pendingLegacy.workspace, "pending.log"), "utf8") === "PENDING"; } catch (error) { if (error.code !== "ENOENT") throw error; } }, "legacy pending native effect");
    let reconnectLegacy;
    pendingLegacy.gate = new Promise(resolve => { reconnectLegacy = resolve; });
    pendingLegacy.socket.terminate();
    const pendingRow = await waitFor(async () => {
      const row = (await ok(request(inventoryPath))).legacy.find(row => row.machine_id === pendingLegacy.id);
      return row && !row.online && row.pending_calls > 0 ? row : undefined;
    }, "offline legacy pending inventory");
    assert.equal(pendingRow.retirable, false);
    assert.deepEqual((await request(retirePath, { machine_id: pendingLegacy.id, runtime_id: pendingRow.runtime_id })).value, { error: "legacy_runtime_has_pending_calls" });
    pendingLegacy.gate = undefined; reconnectLegacy();
    await waitFor(() => pendingLegacy.client.connected, "legacy pending recovery reconnect");
    await writeFile(join(pendingLegacy.workspace, "release-process"), "release");
    assert.equal((await pendingLegacyCall).structuredResult.output, "RECOVERED_OK");
    await pendingLegacy.attachment.close();
    assert.equal((await ok(request(inventoryPath))).legacy.some(row => row.machine_id === pendingLegacy.id), false, "graceful Drain clears the legacy catalog");
    evidence.observed.pending_retirement_rejected = true;
    const activeLegacy = (await ok(request(inventoryPath))).legacy.find(row => row.machine_id === legacy.id);
    assert.equal(activeLegacy.runtime_id, legacy.runtimeId); assert.equal(activeLegacy.online, true);
    assert.equal(activeLegacy.retirable, false);
    const retirement = { machine_id: legacy.id, runtime_id: activeLegacy.runtime_id };
    assert.equal((await request(retirePath, retirement, "ncx_live_invalid")).status, 401);
    assert.equal((await request(retirePath, retirement, restricted)).status, 403);
    assert.equal((await request(retirePath, retirement, otherToken)).status, 409);
    assert.equal((await request(retirePath, retirement)).status, 409, "connected legacy runtime cannot be retired");
    legacy.socket.terminate();
    await waitFor(() => !legacy.client.connected, "legacy transport offline");
    const offlineLegacy = await waitFor(async () => {
      const row = (await ok(request(inventoryPath))).legacy.find(row => row.machine_id === legacy.id);
      return row?.retirable ? row : undefined;
    }, "legacy runtime retirable inventory");
    assert.equal(offlineLegacy.runtime_id, retirement.runtime_id);
    assert.equal(offlineLegacy.online, false); assert.equal(offlineLegacy.pending_calls, 0);
    await assert.rejects(publish({ label: "offline-legacy-collision", id: "legacy-hand_legacy", colo: "SJC", extra: { echo: echo.regional_echo }, reconnect: false }), /catalog_contract_mismatch|collision/);
    evidence.observed.retained_legacy_collision = true;
    const newIdentity = { "x-nanocodex-hand-machine-id": legacy.id, "x-nanocodex-hand-runtime-id": crypto.randomUUID(), "x-fixture-colo": "FRA" };
    await deniedUpgrade(token, 409, newIdentity);
    assert.equal((await request(retirePath, { ...retirement, runtime_id: crypto.randomUUID() })).status, 409);
    assert.deepEqual(await ok(request(retirePath, retirement)), { retired: true, ...retirement });
    assert.deepEqual(await ok(request(retirePath, retirement)), { retired: true, ...retirement }, "exact retirement retry is idempotent");
    const migrated = await publish({ label: "retired-migration", id: legacy.id, colo: "FRA" });
    assert.match((await bind("retired-migration", legacy.id)).route, /^hand-relay:v1:weur:/);
    assert.equal((await invoke("retired-migration", "retired-migration-exec", shell("printf MIGRATED_OK"))).structuredResult.output, "MIGRATED_OK");
    assert.equal((await discover()).data.find(row => row.id === legacy.id).workspace, paths[legacy.id]);
    await deniedUpgrade(token, 409, { ...newIdentity, "x-nanocodex-hand-runtime-id": retirement.runtime_id });
    assert.equal(migrated.client.connected, true, "retired identity cannot replace the admitted successor");
    evidence.observed.public_inventory_retirement = true;
    const pendingRegional = await publish({ label: "pending-regional", id: "pending-regional-hand" });
    await bind("pending-regional", pendingRegional.id);
    const regionalCall = invoke("pending-regional", "pending-regional-call", shell("printf PENDING > pending.log; while [ ! -f release-process ]; do sleep 0.02; done; printf REGIONAL_RECOVERED", 30000));
    regionalCall.catch(() => {});
    await waitFor(async () => { try { return await readFile(join(pendingRegional.workspace, "pending.log"), "utf8") === "PENDING"; } catch (error) { if (error.code !== "ENOENT") throw error; } }, "regional pending native effect");
    let resumeRegional;
    pendingRegional.gate = new Promise(resolve => { resumeRegional = resolve; });
    pendingRegional.socket.terminate();
    const pendingRegionalRow = await waitFor(async () => {
      const row = (await ok(request(inventoryPath))).regional.find(row => row.machine_id === pendingRegional.id);
      return row && !row.online && row.pending_calls > 0 ? row : undefined;
    }, "offline regional pending inventory");
    const regionalIdentity = row => ({ machine_id: row.machine_id, runtime_id: row.runtime_id, publication_id: row.publication_id, region: row.region });
    assert.equal(pendingRegionalRow.retirable, false);
    assert.equal((await request(retirePath, regionalIdentity(pendingRegionalRow))).status, 409);
    pendingRegional.gate = undefined; resumeRegional();
    await waitFor(() => pendingRegional.client.connected, "regional pending recovery reconnect");
    await writeFile(join(pendingRegional.workspace, "release-process"), "release");
    assert.equal((await regionalCall).structuredResult.output, "REGIONAL_RECOVERED");
    await pendingRegional.attachment.close();
    const stoppedRegional = await waitFor(async () => {
      const row = (await ok(request(inventoryPath))).regional.find(row => row.machine_id === pendingRegional.id);
      return row?.retirable ? row : undefined;
    }, "stopped regional publication safe to retire");
    const regionalRetirement = regionalIdentity(stoppedRegional);
    assert.deepEqual(await ok(request(retirePath, regionalRetirement)), { retired: true, ...regionalRetirement });
    assert.deepEqual(await ok(request(retirePath, regionalRetirement)), { retired: true, ...regionalRetirement });
    assert.equal((await ok(request(inventoryPath))).regional.some(row => row.machine_id === pendingRegional.id), false);
    assert.equal((await ok(request("/v1/account/hands/inventory"))).data.some(row => row.id === pendingRegional.id), false);
    evidence.observed.regional_pending_retirement_rejected = true;
    evidence.observed.regional_retirement_clears_directory = true;

    assert.ok(wire.filter(r => r.event === "upgrade").every(r => r.status === 101));
    console.log(JSON.stringify({ evidence: output, ...evidence.observed }));
  } catch (error) { failure = error; evidence.error = error.stack; throw error; }
  finally {
    if (mf && token) await driver({ operation: "admission", input: "release" }).catch(() => {});
    for (const peer of publishers) for (const file of ["release-process", "release-replaced", "release-cached"]) await writeFile(join(peer.workspace, file), "cleanup");
    for (const peer of publishers) { await peer.attachment?.close(); for (const socket of peer.sockets) socket.terminate(); await peer.tools.close(); await peer.native.close(); }
    await mf?.dispose(); console.info = originalInfo;
    await writeFile(join(output, "trace.json"), JSON.stringify({ evidence, http }, null, 2));
    await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2));
    await writeFile(join(output, "runtime.log"), runtime.join("\n"));
    await writeFile(join(output, "README.md"), `Command: ${command}\nInputs: ${JSON.stringify(evidence.inputs)}\nExpected: ${JSON.stringify(evidence.expected)}\nObserved: ${JSON.stringify(evidence.observed)}\nStatus: ${failure ? failure.stack : "PASS"}\nEvidence: trace.json, wire.json, runtime.log, source-resolution.json, worker.mjs, sqlite/, native workspace files. Fixture faults delay admission replies or drop WebSocket frames; production authorization, routing, storage and execution are unchanged. Geography is trusted fixture ingress; Miniflare proves routing identities and transport behavior, not physical placement latency.\n`);
  }
});
