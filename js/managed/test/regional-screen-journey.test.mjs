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

const root = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000091";
const other = "00000000-0000-4000-8000-000000000092";
const machine = "synthetic-screen-mac";
const command = "pnpm --filter nanocodex-managed-service test:regional-screens";

// Ingress geography, account issuance and fault toggles are fixtures. The edge
// proxy (signed viewer fast path), managed Worker routing, owner authority,
// regional relays, DO storage and WebSockets are the production modules.
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
    if(path==='/__fixture/claim/hold') { this.holdNext=true; return Response.json({ok:true}); }
    if(path==='/__fixture/claim/status') return Response.json({held:this.held===true});
    if(path==='/__fixture/claim/release') { this.release?.(); return Response.json({ok:true}); }
    if(path.startsWith('/regional/')||path.startsWith('/hands/')) console.info({type:'fixture.route',target:'owner',path,at:Date.now()});
    if(path==='/regional/screen-claim'&&this.holdNext) {
      // Delay delivery of one relay claim; the production authority decides it afterwards.
      this.holdNext=false; this.held=true; await new Promise(resolve=>{this.release=resolve;}); this.held=false;
    }
    return super.fetch(request);
  }
}
export class ObservedRegionalHandRelay extends RegionalHandRelay {
  async fetch(request) {
    const path=new URL(request.url).pathname;
    if(path==='/__fixture/fence-fail') { this.ctx.storage.kv.put('fixture_fence_fail',(await request.json()).fail); return Response.json({ok:true}); }
    console.info({type:'fixture.route',target:'relay',path,at:Date.now()});
    if(path==='/regional/screen-fence'&&this.ctx.storage.kv.get('fixture_fence_fail')) return Response.json({error:'fixture_unreachable'},{status:503});
    return super.fetch(request);
  }
}
export class FixtureDriver extends DurableObject {
  async fetch(request) {
    const {owner,operation,machine}=await request.json();
    if(operation==='snapshot') return this.env.NANOCODEX_ACCOUNT_TOOLS.getByName(owner).fetch('https://account-tools.internal/snapshot',{method:'POST',body:JSON.stringify({owner_id:owner})});
    const provider=new AccountHostedToolsProvider(this.env.NANOCODEX_ACCOUNT_TOOLS,owner,()=>true,'00000000-0000-7000-8000-000000000093',this.env.NANOCODEX_HAND_RELAYS,new AccountHostedToolsCallRoutes(this.ctx.storage));
    await provider.refresh();
    const tool=provider.screenTool(machine);
    if(!tool) return Response.json({route:null});
    const value=await tool.handler({action:'observe'},{sessionId:'regional-screen-session',callId:'call-'+crypto.randomUUID(),model:'synthetic-model'});
    return Response.json({route:tool.routeToken,value});
  }
}
const ingress={SJC:{continent:'NA',country:'US',longitude:'-121.89'},IAD:{continent:'NA',country:'US',longitude:'-77.45'},FRA:{continent:'EU',country:'DE',longitude:'8.68'}};
export default { async fetch(request,env,ctx) {
  const url=new URL(request.url);
  if(env.EDGE) {
    if(url.pathname.startsWith('/__fixture')) return env.NANOCODEX_BACKEND.fetch(request);
    return await routeManaged(request,env,url,ctx) ?? new Response(null,{status:404});
  }
  if(url.pathname==='/__fixture/issue') {
    const body=await request.json(); await ensureAccount(env,body.owner,true);
    const auth=await (await env.NANOCODEX_USERS.getByName(body.owner).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env,{kind:'api_key',userId:body.owner,...auth.grant,
      subjectId:'api_key:'+body.owner,credentialId:'fixture',capabilities:body.capabilities??auth.grant.capabilities},'Synthetic regional screens'));
  }
  if(url.pathname==='/__fixture/driver') {
    const principal=await authenticate(request,env,url); if(!principal) return new Response(null,{status:401});
    return env.DRIVER.getByName(principal.userId).fetch('https://driver.internal/',{method:'POST',body:JSON.stringify({...await request.json(),owner:principal.userId})});
  }
  if(url.pathname==='/__fixture/fault') {
    const body=await request.json();
    const target=body.target==='relay'?env.NANOCODEX_HAND_RELAYS.getByName(body.owner+':hand-relay:v1:'+body.region):env.NANOCODEX_ACCOUNT_TOOLS.getByName(body.owner);
    return target.fetch('https://account-tools.internal/__fixture/'+body.path,{method:'POST',body:JSON.stringify(body.body??{})});
  }
  // Real clients cannot assign Request.cf; this simulates trusted ingress metadata.
  // An unknown colo has no continent, which selects the legacy owner path.
  // Rollback: the same production Worker with NANOCODEX_REGIONAL_SCREEN_RELAYS off, sharing every DO.
  if(request.headers.get('x-fixture-flag')==='off'&&env.MANAGED_OFF) return env.MANAGED_OFF.fetch(request);
  const colo=request.headers.get('x-fixture-colo')??'LEG';
  return managed.fetch(new Request(request,{cf:{colo,...(ingress[colo]??{})}}),env,ctx);
}};
`;

async function bundleWorker(output) {
  const assets = [];
  const bundle = await build({ stdin: { contents: source, resolveDir: join(root, "js/managed") }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022", external: ["cloudflare:*", "node:*"],
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    alias: { "node-rsa": join(root, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const contents = await readFile(join(args.resolveDir, args.path));
      const name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent" });
  await writeFile(join(output, "fixture-source.mjs"), source);
  await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
  return { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
    modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets] };
}

function startMiniflare(worker, output, runtime) {
  const secret = { NANOCODEX_ACCESS_SECRET: "synthetic-regional-screen-access-secret" };
  const durable = Object.fromEntries([
    ["NANOCODEX_USERS", "UserAccount"], ["NANOCODEX_ORGANIZATIONS", "Organization"], ["NANOCODEX_API_KEYS", "ApiKeyRecord"],
    ["NANOCODEX_AUTH", "NonceStorage"], ["NANOCODEX_ACCOUNT_TOOLS", "ObservedAccountHostedTools"],
    ["NANOCODEX_HAND_RELAYS", "ObservedRegionalHandRelay"], ["NANOCODEX_SCREEN_PLAYBACK", "ScreenPlayback"], ["DRIVER", "FixtureDriver"],
  ].map(([binding, className]) => [binding, { className, useSQLite: true }]));
  return new Miniflare({ port: 0, durableObjectsPersist: join(output, "sqlite"),
    handleRuntimeStdio(stdout, stderr) { for (const stream of [stdout, stderr]) createInterface({ input: stream }).on("line", line => runtime.push(line)); },
    workers: [
      // The account edge: signed viewer admission goes straight to the owner or the generation's relay.
      { ...worker, name: "edge", bindings: { EDGE: true, ...secret }, serviceBindings: { NANOCODEX_BACKEND: "managed" },
        durableObjects: { NANOCODEX_HAND_BROKER: { className: "ObservedAccountHostedTools", scriptName: "managed", useSQLite: true },
          NANOCODEX_HAND_RELAYS: { className: "ObservedRegionalHandRelay", scriptName: "managed", useSQLite: true } } },
      { ...worker, name: "managed", bindings: { NANOCODEX_REGIONAL_HAND_RELAYS: "true", NANOCODEX_REGIONAL_SCREEN_RELAYS: "true", ...secret },
        serviceBindings: { MANAGED_OFF: "managed-off" }, durableObjects: durable },
      { ...worker, name: "managed-off", bindings: { NANOCODEX_REGIONAL_HAND_RELAYS: "true", NANOCODEX_REGIONAL_SCREEN_RELAYS: "false", ...secret },
        durableObjects: Object.fromEntries(Object.entries(durable).map(([binding, value]) => [binding, { ...value, scriptName: "managed" }])) },
    ] });
}

const surface = { id: "display-1", name: "Synthetic display", kind: "desktop", width: 1280, height: 800, controllable: true, agent_tools: true, playback: true };
const jpeg = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAEBAQ==";

/** A protocol-level screen publisher: the same frames the native Mac publisher sends. */
function publisher(ctx, label, { colo, token = ctx.token, id = machine, autoAnswer = true, headers = {} } = {}) {
  const frames = [], peer = { label, frames, closed: undefined };
  const socket = new WebSocket(ctx.ws("/v1/account/hands/host"), { headers: { authorization: `Bearer ${token}`, ...(colo ? { "x-fixture-colo": colo } : {}), ...headers } });
  peer.socket = socket;
  peer.upgrade = new Promise((resolve, reject) => {
    socket.once("upgrade", response => resolve(response.statusCode));
    socket.once("unexpected-response", (_, response) => { response.resume(); resolve(response.statusCode); });
    socket.once("error", reject);
  });
  peer.closedPromise = new Promise(resolve => socket.on("close", (code, reason) => { peer.closed = { code, reason: String(reason) }; ctx.wire.push({ label, event: "close", at: Date.now(), ...peer.closed }); resolve(peer.closed); }));
  socket.on("message", data => {
    const frame = JSON.parse(String(data)); frames.push(frame);
    ctx.wire.push({ label, at: Date.now(), direction: "broker", type: frame.type, ...(frame.type === "signal" ? { signal: frame.signal.type } : {}),
      ...(frame.connection_id ? { connection_id: frame.connection_id, generation: frame.generation } : {}) });
    if (frame.type === "ready") { Object.assign(peer, { connectionId: frame.connection_id, generation: frame.generation });
      socket.send(JSON.stringify({ type: "catalog", machine_id: id, machine_name: "Synthetic Mac", surfaces: [surface] })); }
    if (frame.type === "agent_call" && autoAnswer) socket.send(JSON.stringify({ type: "agent_result", request_id: frame.request_id, status: "ok", jpeg, width: 1, height: 1 }));
  });
  peer.next = (predicate, label2 = "frame") => ctx.until(() => frames.find(predicate), `${label} ${label2}`);
  peer.send = value => socket.send(JSON.stringify(value));
  return peer;
}

function viewer(ctx, label, { generation, headers }) {
  const frames = [], peer = { label, frames };
  const url = ctx.ws(`/v1/account/hands/view?machine_id=${machine}&surface_id=${surface.id}&generation=${encodeURIComponent(generation)}`);
  const socket = new WebSocket(url, { headers });
  peer.socket = socket;
  peer.upgrade = new Promise((resolve, reject) => {
    socket.once("upgrade", response => resolve({ status: response.statusCode, timing: response.headers["server-timing"] }));
    socket.once("unexpected-response", (_, response) => { let body = ""; response.on("data", c => { body += c; }); response.on("end", () => resolve({ status: response.statusCode, body })); });
    socket.once("error", reject);
  });
  peer.closedPromise = new Promise(resolve => socket.on("close", (code, reason) => { peer.closed = { code, reason: String(reason) }; ctx.wire.push({ label, event: "close", at: Date.now(), ...peer.closed }); resolve(peer.closed); }));
  socket.on("message", data => { const frame = JSON.parse(String(data)); frames.push(frame); ctx.wire.push({ label, at: Date.now(), direction: "broker", type: frame.type, ...(frame.type === "signal" ? { signal: frame.signal.type } : {}) }); });
  peer.next = (predicate, what = "frame") => ctx.until(() => frames.find(predicate), `${label} ${what}`);
  peer.send = value => socket.send(JSON.stringify(value));
  return peer;
}

test("regional screen relays keep owner authority, fail closed and route viewers, renewals and agents", { timeout: 180_000 }, async () => {
  const output = join(root, "output/regional-screen-journey", `${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const runtime = [], wire = [], http = [], observed = {};
  const evidence = { command, inputs: { owner, machine, ingress: { SJC: "wnam", FRA: "weur", LEG: "legacy" } },
    expected: { rejected_auth: true, regional_ids: true, legacy_ids: true, signed_fast_path_regional: true, signal_metrics_payload_free: true,
      renew_regional_and_legacy: true, wrong_region_ids_rejected: true, agent_route_regional: true, cross_region_replacement: true,
      fail_closed_unreachable_fence: true, old_viewer_until_fenced: true, recovery_after_fence: true, delayed_claim_not_resurrected: true,
      legacy_replaces_regional: true, owner_revocation: true, playback_command_routed: true, flag_off_retained_regional: true, authority_survives_restart: true }, observed };
  let mf, failure;
  const ctx = { wire, token: undefined, base: undefined,
    ws: path => new URL(path, ctx.base).href.replace(/^http/, "ws"),
    async until(predicate, label, timeout = 10_000) {
      const deadline = performance.now() + timeout;
      while (performance.now() < deadline) { const value = await predicate(); if (value) return value; await delay(20); }
      assert.fail(`timed out: ${label}; evidence: ${output}`);
    } };
  const request = async (path, { method, body, key = ctx.token, headers = {} } = {}) => {
    const response = await fetch(new URL(path, ctx.base), { method: method ?? (body === undefined ? "GET" : "POST"),
      headers: { ...(key ? { authorization: `Bearer ${key}` } : {}), "content-type": "application/json", ...headers },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(20_000) });
    const text = await response.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
    http.push({ path, status: response.status, ...(path.endsWith("/issue") ? {} : { input: body, value }) });
    return { status: response.status, value, headers: response.headers };
  };
  const screens = async () => { const r = await request("/v1/account/hands/screens"); assert.equal(r.status, 200, JSON.stringify(r.value)); return r; };
  const listed = async () => (await screens()).value.surfaces.filter(s => s.machine_id === machine).map(s => s.generation);
  const fault = body => request("/__fixture/fault", { body: { owner, ...body } });
  const published = peer => peer.next(f => f.type === "published", "published");
  const peers = [];
  const host = (label, options) => { const peer = publisher(ctx, label, options); peers.push(peer); return peer; };
  const view = (label, options) => { const peer = viewer(ctx, label, options); peers.push(peer); return peer; };
  try {
    const worker = await bundleWorker(output);
    mf = startMiniflare(worker, output, runtime);
    ctx.base = await mf.ready;
    ctx.token = (await request("/__fixture/issue", { body: { owner } })).value.token;
    const otherToken = (await request("/__fixture/issue", { body: { owner: other } })).value.token;
    const readOnly = (await request("/__fixture/issue", { body: { owner, capabilities: ["agents:read", "tools:use"] } })).value.token;

    // Rejected authentication and authorization never reach a broker.
    assert.equal((await request("/v1/account/hands/screens", { key: "ncx_live_invalid" })).status, 401);
    assert.equal(await publisher(ctx, "invalid-key", { colo: "SJC", token: "ncx_live_invalid" }).upgrade, 401);
    assert.equal(await publisher(ctx, "read-only", { colo: "SJC", token: readOnly }).upgrade, 403);
    observed.rejected_auth = true;

    // Regional and legacy publishers with their identifier formats.
    const a = host("a-wnam", { colo: "SJC" }); assert.equal(await a.upgrade, 101); await published(a);
    assert.match(a.connectionId, /^rs\.wnam\./); assert.match(a.generation, /^rs\.wnam\./);
    observed.regional_ids = true;
    const legacyMachine = publisher(ctx, "legacy-other", { colo: "LEG", id: "synthetic-legacy-mac" }); peers.push(legacyMachine);
    await published(legacyMachine);
    assert.doesNotMatch(legacyMachine.connectionId, /^rs\./);
    const listing = await screens();
    assert.deepEqual(listing.value.surfaces.map(s => s.machine_id).sort(), [machine, "synthetic-legacy-mac"].sort());
    assert.deepEqual((await request("/v1/account/hands/screens", { key: otherToken })).value.surfaces, []);
    observed.legacy_ids = true;

    // Signed viewer admission bypasses the managed hop and lands on the generation's relay.
    const access = listing.headers.get("x-nanocodex-access");
    assert.ok(access, "managed listing issues signed viewer access");
    const av = view("a-viewer", { generation: a.generation, headers: { authorization: `Bearer ${ctx.token}`, "x-nanocodex-access": access } });
    const avUpgrade = await av.upgrade; assert.equal(avUpgrade.status, 101);
    assert.match(String(avUpgrade.timing), /screen_route/);
    await av.next(f => f.type === "ready", "ready");
    const viewerFrame = await a.next(f => f.type === "viewer", "viewer");
    await ctx.until(() => runtime.some(line => line.includes('"type":"hand.proxy"') && line.includes('"route":"local_access_regional"')), "regional fast path proxy log");
    observed.signed_fast_path_regional = true;
    a.send({ type: "signal", viewer_id: viewerFrame.viewer_id, signal: { type: "offer", sdp: "v=0 SYNTHETIC_OFFER_SDP" } });
    a.send({ type: "signal", viewer_id: viewerFrame.viewer_id, signal: { type: "candidate", candidate: "candidate:SYNTHETIC_HOST_CANDIDATE", sdpMid: "0", sdpMLineIndex: 0 } });
    await av.next(f => f.type === "signal" && f.signal.type === "offer", "offer");
    av.send({ type: "signal", signal: { type: "answer", sdp: "v=0 SYNTHETIC_ANSWER_SDP" } });
    av.send({ type: "signal", signal: { type: "candidate", candidate: "candidate:SYNTHETIC_VIEWER_CANDIDATE", sdpMid: "0", sdpMLineIndex: 0 } });
    await a.next(f => f.type === "signal" && f.signal.type === "answer", "answer");
    await a.next(f => f.type === "signal" && f.signal.type === "candidate", "viewer candidate");
    const signalLines = await ctx.until(() => { const lines = runtime.filter(line => line.includes('"stage":"signal.')); return lines.length >= 4 && lines; }, "signal metrics");
    assert.ok(signalLines.every(line => !/SYNTHETIC_|sdp|candidate:/i.test(line)), "signal metrics carry no SDP or candidates");
    assert.ok(signalLines.some(line => line.includes("signal.offer_relayed")) && signalLines.some(line => line.includes("signal.answer_relayed")));
    observed.signal_metrics_payload_free = true;

    // Renewals reach the relay by connection-ID prefix; publication renewals need agents:write.
    assert.equal((await request("/v1/account/hands/renew", { body: { connection_id: a.connectionId } })).status, 200);
    const avReady = av.frames.find(f => f.type === "ready");
    assert.equal((await request("/v1/account/hands/renew", { body: { connection_id: avReady.connection_id } })).status, 200);
    assert.equal((await request("/v1/account/hands/renew", { body: { connection_id: a.connectionId }, key: readOnly })).status, 403);
    assert.equal((await request("/v1/account/hands/renew", { body: { connection_id: legacyMachine.connectionId } })).status, 200);
    observed.renew_regional_and_legacy = true;

    // Wrong or forged region prefixes never find another region's publication.
    const suffix = a.generation.slice("rs.wnam.".length);
    for (const generation of [`rs.apac.${suffix}`, `rs.zz.${suffix}`, suffix]) {
      const wrong = view(`wrong-${generation.slice(0, 7)}`, { generation, headers: { authorization: `Bearer ${ctx.token}` } });
      assert.equal((await wrong.upgrade).status, 409, generation);
    }
    assert.equal((await request("/v1/account/hands/renew", { body: { connection_id: `rs.apac.${a.connectionId.slice(8)}` } })).status, 409);
    observed.wrong_region_ids_rejected = true;

    // Agent CUA: the owner snapshot lists the regional screen and routes its tool through that relay.
    const agent = await request("/__fixture/driver", { body: { operation: "invoke", machine } });
    assert.equal(agent.status, 200, JSON.stringify(agent.value));
    assert.match(agent.value.route, /^hand-relay:v1:wnam:screen:v1:/);
    assert.equal(agent.value.value?.structuredResult?.status ?? agent.value.value?.status, "ok", JSON.stringify(agent.value.value).slice(0, 400));
    assert.ok(a.frames.some(f => f.type === "agent_call"));
    observed.agent_route_regional = true;

    // Cross-region replacement fences the old publisher and its viewer before the new one is listed.
    const b = host("b-weur", { colo: "FRA" }); await published(b);
    assert.match(b.generation, /^rs\.weur\./);
    assert.match((await a.closedPromise).reason, /Host replaced/);
    await av.closedPromise;
    assert.deepEqual(await listed(), [b.generation]);
    assert.equal((await view("stale-a", { generation: a.generation, headers: { authorization: `Bearer ${ctx.token}` } }).upgrade).status, 409);
    observed.cross_region_replacement = true;

    // Unreachable old relay: the replacement is rejected and nothing is listed until the fence is confirmed.
    const bv = view("b-viewer", { generation: b.generation, headers: { authorization: `Bearer ${ctx.token}` } });
    assert.equal((await bv.upgrade).status, 101); const bViewer = await b.next(f => f.type === "viewer", "viewer");
    assert.equal((await fault({ target: "relay", region: "weur", path: "fence-fail", body: { fail: true } })).status, 200);
    const c = host("c-wnam-rejected", { colo: "SJC" });
    assert.match((await c.closedPromise).reason, /Host publication rejected/);
    assert.ok(!c.frames.some(f => f.type === "published"));
    assert.deepEqual(await listed(), [], "fail closed while the old location is unconfirmed");
    // The old publication and viewer still exchange control frames: no second publication exists.
    b.send({ type: "signal", viewer_id: bViewer.viewer_id, signal: { type: "offer", sdp: "v=0 SYNTHETIC_OFFER_SDP" } });
    await bv.next(f => f.type === "signal", "old viewer signal while fence pending");
    assert.equal(b.closed, undefined); assert.equal(bv.closed, undefined);
    observed.fail_closed_unreachable_fence = true; observed.old_viewer_until_fenced = true;
    await fault({ target: "relay", region: "weur", path: "fence-fail", body: { fail: false } });
    await listed(); // Listing retries retained fences outside any claim.
    assert.match((await b.closedPromise).reason, /Host replaced/);
    await bv.closedPromise;
    const c2 = host("c2-wnam", { colo: "SJC" }); await published(c2);
    assert.deepEqual(await listed(), [c2.generation]);
    observed.recovery_after_fence = true;

    // A claim delayed in flight from a publisher fenced meanwhile can never become authority.
    await fault({ target: "owner", path: "claim/hold" });
    const d = host("d-wnam-delayed", { colo: "SJC" });
    await ctx.until(async () => (await fault({ target: "owner", path: "claim/status" })).value.held, "claim held");
    const e = host("e-weur", { colo: "FRA" }); await published(e);
    assert.match((await c2.closedPromise).reason, /Host replaced/);
    assert.match((await d.closedPromise).reason, /Host replaced/, "fenced while its claim was in flight");
    const decisions = () => runtime.filter(line => line.includes('"type":"hand.screen.claim"')).map(line => JSON.parse(line.slice(line.indexOf("{"))));
    const before = decisions().length;
    await fault({ target: "owner", path: "claim/release" });
    const late = await ctx.until(() => decisions().slice(before).find(record => record.region === "wnam"), "late claim decision");
    assert.equal(late.granted, false, "watermark rejects the fenced publisher's delayed claim");
    assert.ok(!d.frames.some(f => f.type === "published"));
    assert.equal(e.closed, undefined);
    assert.deepEqual(await listed(), [e.generation]);
    observed.delayed_claim_not_resurrected = { sequence: late.sequence };

    // Legacy owner publication of the same machine fences the regional one.
    const l2 = host("l2-legacy", { colo: "LEG" }); await published(l2);
    assert.doesNotMatch(l2.generation, /^rs\./);
    assert.match((await e.closedPromise).reason, /Host replaced/);
    assert.deepEqual(await listed(), [l2.generation]);
    observed.legacy_replaces_regional = true;

    // Owner removal withdraws screen authority in every location; a later publication is fresh.
    const forgot = await request(`/v1/account/hands/${machine}?force=1`, { method: "DELETE" });
    assert.equal(forgot.status, 200, JSON.stringify(forgot.value));
    assert.match((await l2.closedPromise).reason, /Hand revoked/);
    assert.deepEqual(await listed(), []);
    const r = host("r-wnam", { colo: "SJC" }); await published(r);
    assert.deepEqual(await listed(), [r.generation]);
    observed.owner_revocation = true;

    // Portable playback commands reach the authoritative regional host socket.
    const link = await request("/v1/account/hands/playback-links", { body: { operation_id: crypto.randomUUID(), machine_id: machine, surface_id: surface.id, generation: r.generation } });
    const start = await r.next(f => f.type === "broadcast" && f.target === "hls" && f.action === "start", "hls start");
    assert.match(start.upload.token, /^nsu_/); assert.match(start.upload.url, /\/upload\/$/);
    observed.playback_command_routed = { status: link.status };

    // Rollback with a retained regional host: the flag only stops placing NEW hosts regionally.
    const off = { "x-fixture-flag": "off" };
    const offListing = await request("/v1/account/hands/screens", { headers: off });
    assert.deepEqual(offListing.value.surfaces.filter(s => s.machine_id === machine).map(s => s.generation), [r.generation]);
    const rv = view("r-viewer-flag-off", { generation: r.generation, headers: { authorization: `Bearer ${ctx.token}`, ...off } });
    assert.equal((await rv.upgrade).status, 101); await r.next(f => f.type === "viewer", "viewer while flag off");
    assert.equal((await request("/v1/account/hands/renew", { body: { connection_id: r.connectionId }, headers: off })).status, 200);
    const g = host("g-flag-off", { colo: "SJC", headers: off }); await published(g);
    assert.doesNotMatch(g.generation, /^rs\./, "new hosts use the owner while the flag is off");
    assert.match((await r.closedPromise).reason, /Host replaced/);
    assert.deepEqual((await request("/v1/account/hands/screens", { headers: off })).value.surfaces.filter(s => s.machine_id === machine).map(s => s.generation), [g.generation]);
    observed.flag_off_retained_regional = true;

    // Durable authority survives a full runtime restart; sockets do not.
    for (const peer of peers) peer.socket.terminate();
    await mf.dispose(); mf = startMiniflare(worker, output, runtime); ctx.base = await mf.ready;
    assert.deepEqual(await listed(), []);
    assert.equal((await view("after-restart", { generation: r.generation, headers: { authorization: `Bearer ${ctx.token}` } }).upgrade).status, 409);
    const f = host("f-weur", { colo: "FRA" }); await published(f);
    assert.deepEqual(await listed(), [f.generation]);
    observed.authority_survives_restart = true;
    console.log(JSON.stringify({ evidence: output, ...observed }));
  } catch (error) { failure = error; evidence.error = error.stack; throw error; }
  finally {
    for (const peer of peers) peer.socket.terminate();
    await mf?.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({ evidence, http }, null, 2));
    await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2));
    await writeFile(join(output, "runtime.log"), runtime.join("\n"));
    await writeFile(join(output, "README.md"), `Command: ${command}\nInputs: ${JSON.stringify(evidence.inputs)}\nExpected: ${JSON.stringify(evidence.expected)}\nObserved: ${JSON.stringify(observed)}\nStatus: ${failure ? failure.stack : "PASS"}\nEvidence: trace.json (HTTP), wire.json (WebSocket frame types, no SDP), runtime.log (hand.request/hand.proxy/hand.remote records), worker.mjs, sqlite/. Fixtures: ingress geography, account issuance, one held claim delivery and one failing relay fence. Miniflare proves routing, authority and transport behavior, not physical placement latency.\n`);
  }
});
