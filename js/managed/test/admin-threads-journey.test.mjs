import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { appendFileSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";

// Reproduce: pnpm --filter nanocodex-managed-service run test:admin-threads
// Only identity enrollment and the external model are fixtures. Public HTTP,
// authentication, account discovery, session events and exec run in workerd.
const root = fileURLToPath(new URL("..", import.meta.url));
const evidenceRoot = join(root, "../../output/admin-threads-journey", `${Date.now()}-${process.pid}`);
const admin = "11111111-1111-4111-8111-111111111101";
const alice = "11111111-1111-4111-8111-111111111102";
const bob = "11111111-1111-4111-8111-111111111103";
const caps = ["agents:read", "agents:write", "history:read", "tools:use"];
const source = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession as BaseSession, AccountHostedTools } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, authenticate } from './src/account-auth.ts';
import { Kv } from 'accounts/server';
export class DurableAgentSession extends BaseSession {
  async recoveryFixture(seed) {
    const sql = this.ctx.storage.sql;
    if (seed === true) {
      const id = sql.exec('SELECT id FROM managed_turns ORDER BY rowid DESC LIMIT 1').one().id;
      sql.exec('UPDATE managed_recovery_safety SET armed=1,abrupt_attempts=4,stopped=1 WHERE turn_id=?',id);
      for (let i=0;i<3;i++) {
        const key='snapshot-effect-'+i;
        sql.exec("INSERT INTO managed_code_effects(effect_key,session_id,turn_id,parent_call_id,call_id,name,input_hash,generation,state,receipt_chunks,created_at,operation_id,model_call_index,scope_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
          key,sql.exec('SELECT session_id FROM nanocodex_cloudflare_agent WHERE singleton=1').one().session_id,id,'parent-'+i,'nested-'+i,'exec_command','PRIVATE_INPUT_SENTINEL','PRIVATE_GENERATION_SENTINEL',i===2?'pending':'completed',i===2?null:i===1?300:1,Date.now(),id,7,2);
        if(i!==2) for(let chunk=0;chunk<(i===1?300:1);chunk++) sql.exec('INSERT INTO managed_code_effect_receipt_chunks VALUES(?,?,?)',key,chunk,'PRIVATE_RECEIPT_SENTINEL'.padEnd(i===0?65536:330,'x'));
      }
    }
    if(seed === 'noise') {
      const id=sql.exec('SELECT id FROM managed_turns ORDER BY rowid DESC LIMIT 1').one().id;
      for(let i=0;i<120;i++) sql.exec("INSERT INTO managed_code_effects(effect_key,session_id,turn_id,parent_call_id,call_id,name,input_hash,generation,state,created_at,operation_id,model_call_index,scope_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        'child-noise-'+i,'child-session',id,'noise-parent','noise-'+i,'exec_command','PRIVATE_INPUT_SENTINEL','PRIVATE_GENERATION_SENTINEL','pending',Date.now(),id,8,2);
    }
    if(seed === 'receipt-schema-break') sql.exec('ALTER TABLE managed_code_effect_receipt_chunks RENAME COLUMN chunk_index TO retained_chunk_index');
    if(seed === 'receipt-schema-restore') sql.exec('ALTER TABLE managed_code_effect_receipt_chunks RENAME COLUMN retained_chunk_index TO chunk_index');
    if(seed === 'effect-schema-break') sql.exec('ALTER TABLE managed_code_effects RENAME COLUMN operation_id TO retained_operation_id');
    if(seed === 'effect-schema-restore') sql.exec('ALTER TABLE managed_code_effects RENAME COLUMN retained_operation_id TO operation_id');
    const values=['managed_turns','managed_recovery_safety','managed_code_effects','managed_code_effect_receipt_chunks','nanocodex_durable_states','nanocodex_durable_owners'].map(table=>sql.exec('SELECT * FROM '+table+' ORDER BY rowid').toArray());
    const digest=await crypto.subtle.digest('SHA-256',new TextEncoder().encode(JSON.stringify(values)));
    return Array.from(new Uint8Array(digest),byte=>byte.toString(16).padStart(2,'0')).join('');
  }
}
export { AccountHostedTools, UserAccount, Organization, ApiKeyRecord, NonceStorage };
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureSandbox extends DurableObject {
  async clearRemoteDesktop() {}
  async destroy() {}
}
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept();
    let index = 0, lastUser;
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', async event => {
      const body = JSON.parse(event.data);
      const user = JSON.stringify((body.input ?? []).filter(item => item.role === 'user').at(-1)) ?? '';
      if (user.includes('ACTIVE_THREAD_HISTORY')) await new Promise(resolve => setTimeout(resolve, 1500));
      const marker = user.match(/ADMIN_TOOL (accounts|read|diagnostics|performance|restricted_read|bigoutput)(?: ([0-9a-f-]{36}))?/);
      if (marker && marker[0] !== lastUser) { index = 0; lastUser = marker[0]; }
      const operation = marker?.[1] === 'restricted_read' ? 'read' : marker?.[1];
      const args = {operation, limit: operation === 'accounts' ? 2 : 100, ...(operation === 'diagnostics' ? {after_managed:0} : {}), ...(marker?.[2] ? {thread_id:marker[2]} : {})};
      const input = marker && index++ === 0 ? operation === 'bigoutput' ? 'text("BIG_OUTPUT_".repeat(40000))' : 'try { text(await tools.admin_threads('+JSON.stringify(args)+')); } catch (error) { text({denied:String(error)}); }' : undefined;
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_'+crypto.randomUUID(),status:'completed',end_turn:!input,
        output:input ? [{type:'custom_tool_call',name:'exec',call_id:'admin-'+index,input}]
          : [{type:'message',role:'assistant',content:[{type:'output_text',text:'JOURNEY_DONE'}]}],
        usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default { async fetch(request, env, ctx) {
  if (new URL(request.url).pathname === '/__fixture/recovery') {
    const b=await request.json(); return Response.json(await env.NANOCODEX_SESSIONS.getByName(b.id).recoveryFixture(b.seed));
  }
  if (new URL(request.url).pathname === '/__fixture/legacy') {
    await Kv.durableObject(env.NANOCODEX_AUTH,{name:'sms-otp'}).set('identity:synthetic-phone-digest', {userId:'${bob}'});
    await Kv.durableObject(env.NANOCODEX_AUTH,{name:'admin-directory'}).delete('admin-account:${bob}');
    return new Response(null,{status:204});
  }
  if (new URL(request.url).pathname === '/__fixture/principal') return Response.json({kind:(await authenticate(request,env,new URL(request.url)))?.kind});
  if (new URL(request.url).pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    const key = await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,
      credentialId:'fixture',capabilities:b.capabilities},'Synthetic admin journey');
    const token = 's_' + crypto.randomUUID().replaceAll('-','') + 'A'.repeat(11);
    await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,
      {userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
    return Response.json({...key,cookie:'nanocodex_account='+token});
  }
  return worker.fetch(request,env,ctx);
}};
`;

for (const configured of [true, false]) test(`admin thread journey (configured=${configured})`, { timeout: 120_000 }, async () => {
  const output = join(evidenceRoot, configured ? "configured" : "disabled");
  await mkdir(output, { recursive: true });
  const trace = [], wire = [], logs = [], assets = [], provenance = [];
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = join(args.resolveDir, args.path), contents = await readFile(path);
      const name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent",
  });
  const proxy = await build({ stdin: { contents: `import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(request,env){
      const url=new URL(request.url);
      if(url.pathname.startsWith('/__connect/')) {url.protocol='https:';url.host='nanocodex.internal';url.port='';url.pathname=url.pathname.slice(10);return env.NANOCODEX_BACKEND.fetch(new Request(url,request));}
      return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, resolveDir: root },
    bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    external: ["cloudflare:*", "node:*"], logLevel: "silent" });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
    createInterface({ input: stdout }).on("line", line => { logs.push(line); appendFileSync(join(output, "runtime.log"), line + "\n"); });
    createInterface({ input: stderr }).on("line", line => { logs.push(line); appendFileSync(join(output, "runtime.log"), line + "\n"); });
  }, workers: [
    { ...common, name: "account", modules: true, script: proxy.outputFiles[0].text, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { ...common, name: "managed", modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
      durableObjects: {
        NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
        NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
        NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
        NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
        NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
        NANOCODEX_SANDBOXES: { className: "FixtureSandbox", useSQLite: true },
        NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
        NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true },
        MODEL: { className: "FixtureModel", useSQLite: true },
      }, bindings: configured ? { NANOCODEX_ADMIN_USER_ID: admin } : {}, serviceBindings: { NANOCODEX: "provider" },
      r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES", "NANOCODEX_USER_DATA_OBJECTS"] },
    { ...common, name: "provider", modules: true,
      script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}};",
      durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
  ] });
  const sockets = [];
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed");
    const keys = {};
    for (const user of [admin, alice, bob]) {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities: caps }) });
      assert.equal(response.status, 200, await response.clone().text());
      keys[user] = await response.json();
    }
    let principal = admin;
    async function call(path, method = "GET", body, expected = 200, headers) {
      const response = await fetch(new URL(path, base), { signal: AbortSignal.timeout(15_000), method, headers: {
        "content-type": "application/json", authorization: "Bearer " + keys[principal].token, ...headers,
      }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const raw = await response.text(); let data; try { data = JSON.parse(raw); } catch { data = raw; }
      trace.push({ principal, path, method, expected, status: response.status, data });
      appendFileSync(join(output, "http.jsonl"), JSON.stringify(trace.at(-1)) + "\n");
      assert.equal(response.status, expected, method + " " + path + ": " + raw); return data;
    }
    const inspect = (operation, params = {}, expected = 200, headers) => call("/v1/admin/threads?" + new URLSearchParams({ operation, ...params }), "GET", undefined, expected, headers);
    const create = async () => (await call("/v1/agents", "POST", { settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } }, 201)).agent_id;
    if (!configured) {
      await inspect("accounts", {}, 403);
      await inspect("diagnostics", {thread_id:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}, 403);
      return;
    }
    principal = alice;
    const aliceThreads = [await create(), await create(), await create()];
    principal = bob;
    const bobThread = await create();
    principal = admin;
    const adminThread = await create();
    async function turn(id, input, token = keys[principal].token, whileRunning) {
      const frames = [];
      const socket = new WebSocket(new URL(`/v1/agents/${id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: "Bearer " + token } });
      sockets.push(socket);
      socket.on("message", data => { const frame = JSON.parse(String(data)); frames.push(frame); wire.push({ thread: id, ...frame }); appendFileSync(join(output, "wire.jsonl"), JSON.stringify(wire.at(-1)) + "\n"); });
      let error;
      socket.on("error", value => { error = value; });
      async function waitFor(predicate) {
        const deadline = Date.now() + 45_000;
        while (!predicate()) {
          if (error) throw error;
          assert.ok(Date.now() < deadline, "WebSocket timeout: " + JSON.stringify(frames.slice(-6)));
          await delay(20);
        }
      }
      await waitFor(() => frames.some(frame => frame.type === "ready"));
      const turnId = crypto.randomUUID();
      socket.send(JSON.stringify({ type: "prompt", id: turnId, input }));
      if (whileRunning) {
        await waitFor(() => frames.some(frame => frame.event?.type === "model.call.started"));
        assert.ok(!frames.some(frame => frame.id === turnId && frame.type === "turn_completed"));
        await whileRunning();
        assert.ok(!frames.some(frame => frame.id === turnId && frame.type === "turn_completed"));
      }
      await waitFor(() => frames.some(frame => frame.id === turnId && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type)));
      const terminal = frames.find(frame => frame.id === turnId && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type));
      assert.equal(terminal.type, "turn_completed", JSON.stringify(terminal));
      socket.close();
      return frames;
    }
    principal = alice;
    await turn(aliceThreads[0], "ALICE_PRIVATE_HISTORY synthetic schedule");
    principal = bob;
    await turn(bobThread, "BOB_PRIVATE_HISTORY synthetic notes");
    await inspect("accounts", {}, 403);
    await inspect("read", { thread_id: aliceThreads[0] }, 403);
    await inspect("performance", { thread_id: aliceThreads[0] }, 403);
    await inspect("diagnostics", { thread_id: aliceThreads[0] }, 403);
    await call(`/v1/agents/${aliceThreads[0]}/events/history`, "GET", undefined, 404);
    principal = admin;
    const connectHeaders = { authorization: "", "x-nanocodex-connect-user": admin,
      "x-nanocodex-connect-grant-id": "0x" + "a".repeat(64), "x-nanocodex-connect-capabilities": JSON.stringify(caps),
      "x-nanocodex-connect-connectors": "[]", "x-nanocodex-connect-mcp-ids": "[]" };
    assert.equal((await call("/__connect/__fixture/principal", "GET", undefined, 200, connectHeaders)).kind, "connect_grant");
    await call("/__connect/v1/admin/threads?operation=accounts", "GET", undefined, 403, connectHeaders);
    assert.equal((await backend.fetch("https://fixture.test/__fixture/legacy")).status, 204);
    const directoryPage = await inspect("accounts", {limit:100});
    assert.ok(!directoryPage.data.some(row => row.owner_id === bob), "legacy owner absent from new directory");
    const owners = [];
    let cursor;
    do {
      const page = await inspect("accounts", { limit: 1, ...(cursor ? { cursor } : {}) });
      assert.ok(page.data.length <= 1);
      owners.push(...page.data.map(row => row.owner_id));
      cursor = page.next_cursor;
      assert.ok(owners.length <= 10, "accounts pagination must terminate");
    } while (cursor);
    assert.equal(new Set(owners).size, owners.length);
    trace.push({case:"retained_sms_identity_discovery", expected:"legacy owner found without directory entry", owner_id:bob, found:owners.includes(bob)});
    for (const user of [admin, alice, bob]) assert.ok(owners.includes(user), "discover synthetic owner " + user);
    const threads = [];
    cursor = undefined;
    do {
      const page = await inspect("list", { owner_id: alice, limit: 1, ...(cursor ? { cursor } : {}) });
      assert.ok(page.data.length <= 1);
      threads.push(...page.data.map(row => row.id));
      cursor = page.next_cursor;
      assert.ok(threads.length <= 10, "thread pagination must terminate");
    } while (cursor);
    assert.deepEqual(threads.sort(), [...aliceThreads].sort());
    assert.deepEqual((await inspect("list", { owner_id: bob })).data.map(row => row.id), [bobThread]);
    await turn(aliceThreads[2], "ACTIVE_THREAD_HISTORY synthetic pending model", keys[alice].token, async () => {
      const active = await inspect("read", {thread_id:aliceThreads[2]});
      assert.match(JSON.stringify(active.data), /ACTIVE_THREAD_HISTORY/);
      trace.push({case:"active_thread_read", expected:"history readable before model reply", thread_id:aliceThreads[2]});
    });
    const read = await inspect("read", { thread_id: aliceThreads[0] });
    assert.equal(read.thread.id, aliceThreads[0]);
    assert.equal(read.thread.owner_id, alice);
    assert.match(JSON.stringify(read.data), /ALICE_PRIVATE_HISTORY/);
    assert.doesNotMatch(JSON.stringify(read.data), /BOB_PRIVATE_HISTORY/);
    const latest = await inspect("read", { thread_id: aliceThreads[0], limit: 1 });
    assert.equal(latest.data.length, 1);
    assert.equal(latest.has_more, true);
    assert.ok(latest.next_before);
    const earlier = await inspect("read", { thread_id: aliceThreads[0], limit: 1, before: latest.next_before });
    assert.equal(earlier.data.length, 1);
    assert.notDeepEqual(earlier.data, latest.data);
    const after = await inspect("read", { thread_id: aliceThreads[0], after: 0, limit: 1 });
    assert.equal(after.data.length, 1);
    assert.ok(after.next_after);
    const following = await inspect("read", { thread_id: aliceThreads[0], after: after.next_after, limit: 1 });
    assert.equal(following.data.length, 1);
    assert.notDeepEqual(following.data, after.data);
    await call("/__connect/v1/admin/threads?operation=diagnostics&thread_id="+aliceThreads[0], "GET", undefined, 403, connectHeaders);
    const recoveryFixture = async seed => (await backend.fetch("https://fixture.test/__fixture/recovery", {method:"POST",body:JSON.stringify({id:aliceThreads[0],seed})})).json();
    const recoveryBefore = await recoveryFixture(true);
    const diagnostics = await inspect("diagnostics", { thread_id: aliceThreads[0], limit: 1 });
    assert.equal(diagnostics.thread_id, aliceThreads[0]);
    assert.equal(diagnostics.recovery.available,true);
    assert.equal(diagnostics.recovery.turns.data[0].abrupt_attempts,4);
    assert.equal(diagnostics.recovery.turns.data[0].stopped,1);
    assert.equal(diagnostics.recovery.effects.data.length,1);
    assert.equal(diagnostics.recovery.effects.has_more,true);
    assert.equal(diagnostics.recovery.effects.data[0].state,"pending");
    const expanded = await inspect("diagnostics",{thread_id:aliceThreads[0],limit:100});
    assert.equal(expanded.recovery.effects.data.length,3);
    assert.equal(expanded.recovery.effects.data[1].receipt.observed_chunks,257);
    assert.equal(expanded.recovery.effects.data[1].receipt.truncated,true);
    assert.equal(expanded.recovery.effects.data[2].receipt.observed_chunks,1);
    assert.equal(expanded.recovery.effects.data[2].receipt.observed_bytes,undefined);
    assert.doesNotMatch(JSON.stringify(expanded.recovery),/PRIVATE_|input_hash|generation|receipt_json|effect_key/);
    assert.equal(await recoveryFixture(false),recoveryBefore,"inspection must preserve safety, effects, turns, durable heads and owners");
    const noiseBefore=await recoveryFixture('noise');
    const busy=await inspect("diagnostics",{thread_id:aliceThreads[0],limit:100});
    assert.equal(busy.recovery.effects.data.length,100);
    assert.ok(busy.recovery.effects.data.every(effect=>effect.session_id==='child-session'));
    assert.equal(busy.recovery.stopped_root_effects.available,true);
    assert.equal(busy.recovery.stopped_root_effects.data.length,1);
    assert.equal(busy.recovery.stopped_root_effects.data[0].data.length,3);
    assert.ok(busy.recovery.stopped_root_effects.data[0].data.every(effect=>effect.session_id!=='child-session'));
    assert.equal(await recoveryFixture(false),noiseBefore);
    assert.doesNotMatch(JSON.stringify(busy.recovery),/PRIVATE_|input_hash|generation|receipt_json|effect_key/);
    // Real SQLite incompatibility, not a mocked helper: a broken receipt
    // query must leave root safety and effect identity readable, redact its
    // SQL error, and leave all stored evidence unchanged.
    const brokenBefore=await recoveryFixture('receipt-schema-break');
    const partial=await inspect("diagnostics",{thread_id:aliceThreads[0],limit:100});
    assert.equal(partial.recovery.available,true);
    assert.equal(partial.recovery.turns.data[0].abrupt_attempts,4);
    assert.equal(partial.recovery.effects.data.length,100);
    assert.equal(partial.recovery.stopped_root_effects.data[0].data.length,3);
    assert.ok(partial.recovery.effects.data.every(effect=>effect.receipt.available===false));
    assert.equal(partial.recovery.effects.data[0].receipt.reason,"receipt_metadata_unavailable");
    assert.equal(await recoveryFixture(false),brokenBefore);
    assert.doesNotMatch(JSON.stringify(partial.recovery),/PRIVATE_|SQLITE|no such|retained_chunk_index/);
    await recoveryFixture('receipt-schema-restore');
    await recoveryFixture('effect-schema-break');
    const staged=await inspect("diagnostics",{thread_id:aliceThreads[0],limit:10});
    assert.equal(staged.recovery.available,false);
    assert.equal(staged.recovery.stage,"effects");
    assert.doesNotMatch(JSON.stringify(staged.recovery),/PRIVATE_|SQLITE|no such|retained_operation_id/);
    await recoveryFixture('effect-schema-restore');
    principal=alice;
    const ownerDiagnostics=await call(`/v1/agents/${aliceThreads[0]}/diagnostics`);
    assert.equal(ownerDiagnostics.recovery,undefined,"recovery snapshot remains admin-only");
    principal=admin;
    trace.push({case:"recovery_snapshot_after_child_noise",before:noiseBefore,after:await recoveryFixture(false),snapshot:busy.recovery});
    assert.ok(Array.isArray(diagnostics.services));
    assert.ok(diagnostics.services.some(service => service.events.length > 0));
    const offsets = Object.fromEntries(diagnostics.services.map(service => [service.service, service.next_after]));
    await inspect("diagnostics", { thread_id: aliceThreads[0], after_managed: offsets["managed"] ?? 0, after_hand: offsets["hand.broker"] ?? 0 });
    for (const path of [`/v1/agents/${aliceThreads[0]}`, `/v1/agents/${aliceThreads[0]}/events/history`, `/v1/agents/${aliceThreads[0]}/diagnostics`])
      await call(path, "GET", undefined, 404);
    await call(`/v1/agents/${aliceThreads[0]}`, "DELETE", undefined, 404);
    await inspect("accounts", {}, 200, { authorization: "", cookie: keys[admin].cookie, origin: base.origin });
    await inspect("accounts", { cursor: "invalid-cursor" }, 400);
    await inspect("list", { owner_id: alice, cursor: "invalid-cursor" }, 400);
    for (const params of [{ limit: 0 }, { limit: 101 }, { limit: "banana" }]) await inspect("accounts", params, 400);
    await inspect("list", {}, 400);
    await inspect("read", {}, 400);
    await inspect("read", { thread_id: aliceThreads[0], after: -1 }, 400);
    await inspect("read", { thread_id: aliceThreads[0], before: 1, after: 0 }, 400);
    await inspect("unsupported", {}, 400);
    for (const method of ["POST", "PUT", "DELETE"]) await call("/v1/admin/threads?operation=accounts", method, undefined, 405);
    const unknown = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    await inspect("read", { thread_id: unknown }, 404);
    await inspect("diagnostics", { thread_id: unknown }, 404);
    await inspect("performance", { thread_id: unknown }, 404);
    let restrictedAdminToken;
    for (const missing of ["agents:read", "history:read", "tools:use"]) {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user: admin, capabilities: caps.filter(cap => cap !== missing) }) });
      assert.equal(response.status, 200);
      const reduced = await response.json();
      if (missing === "history:read") restrictedAdminToken = reduced.token;
      await inspect("accounts", {}, 403, { authorization: "Bearer " + reduced.token });
    }
    const frames = await turn(adminThread, "ADMIN_TOOL accounts");
    const results = frames.filter(frame => frame.event?.type === "tool.result" && frame.event.payload.tool === "admin_threads").map(frame => frame.event.payload);
    assert.equal(results.length, 1, JSON.stringify(frames));
    assert.equal(results[0].status, "completed", JSON.stringify(results));
    assert.ok(Array.isArray(results[0].structured_result.data), JSON.stringify(results));
    assert.equal(results[0].structured_result.data.length, 2);
    const toolResults = frames => frames.filter(frame => frame.event?.type === "tool.result" && frame.event.payload.tool === "admin_threads").map(frame => frame.event.payload);
    for (const operation of ["read", "diagnostics", "performance"]) {
      const frames = await turn(adminThread, `ADMIN_TOOL ${operation} ${aliceThreads[0]}`);
      const results = toolResults(frames);
      assert.equal(results.length, 1, JSON.stringify(frames));
      assert.equal(results[0].status, "completed", JSON.stringify(results));
      const result = results[0].structured_result;
      if (operation === "read") {
        assert.equal(result.thread.owner_id, alice);
        assert.match(JSON.stringify(result.data), /ALICE_PRIVATE_HISTORY/);
        assert.doesNotMatch(JSON.stringify(result.data), /BOB_PRIVATE_HISTORY/);
      } else if (operation === "performance") {
        assert.equal(result.thread_id, aliceThreads[0]);
        assert.equal(result.settings.model, "gpt-6.1-sol");
        assert.equal(result.capacity.available, true);
        assert.ok(result.capacity.database_size_bytes > 0);
        assert.equal(result.provider_telemetry.available, true);
        assert.equal(result.provider_telemetry.scope, "thread");
        assert.ok(Array.isArray(result.provider_telemetry.groups));
        assert.match(result.evidence.limitation, /unknown/);
      } else {
        assert.equal(result.thread_id, aliceThreads[0]);
        assert.ok(result.services.some(service => service.events.length > 0));
      }
      trace.push({ case: "model_exec_admin_" + operation, expected: "cross-owner access", result });
    }
    const selfFrames = await turn(adminThread, `ADMIN_TOOL read ${adminThread}`);
    const selfResults = toolResults(selfFrames);
    assert.equal(selfResults.length, 1, JSON.stringify(selfFrames));
    assert.equal(selfResults[0].status, "completed", JSON.stringify(selfResults));
    assert.equal(selfResults[0].structured_result.thread.id, adminThread);
    trace.push({case:"model_exec_active_self_read", expected:"current thread readable during its own tool call"});
    // A self read is served in-process and returns a bounded page: an oversized
    // event is a preview with its original size, never a multi-MB tool result.
    await turn(adminThread, "ADMIN_TOOL bigoutput");
    const boundedResults = toolResults(await turn(adminThread, `ADMIN_TOOL read ${adminThread}`));
    assert.equal(boundedResults[0].status, "completed", JSON.stringify(boundedResults).slice(0, 2000));
    const boundedPage = boundedResults[0].structured_result;
    const previews = boundedPage.data.filter(event => event.truncated === true);
    assert.ok(previews.length > 0 && previews.every(event => event.original_bytes > 32 * 1024 && event.preview.length <= 16 * 1024),
      JSON.stringify(boundedPage).slice(0, 2000));
    const preview = previews.find(event => /BIG_OUTPUT_BIG_OUTPUT_/.test(event.preview));
    assert.ok(preview && preview.original_bytes > 400_000, JSON.stringify(previews.map(event => event.original_bytes)));
    assert.ok(JSON.stringify(boundedPage).length < 600_000, String(JSON.stringify(boundedPage).length));
    trace.push({case:"model_exec_self_read_bounded", expected:"oversized self events are previews", original_bytes: preview.original_bytes, page_bytes: JSON.stringify(boundedPage).length});
    for (const operation of ["diagnostics", "performance"]) {
      const selfInspection = toolResults(await turn(adminThread, `ADMIN_TOOL ${operation} ${adminThread}`));
      assert.equal(selfInspection[0].status, "completed", JSON.stringify(selfInspection).slice(0, 2000));
      assert.equal(selfInspection[0].structured_result.thread_id, adminThread);
      trace.push({case:"model_exec_self_" + operation, expected:"own thread inspected in-process"});
    }
    const restrictedFrames = await turn(adminThread, `ADMIN_TOOL restricted_read ${aliceThreads[0]}`, restrictedAdminToken);
    const restrictedResults = toolResults(restrictedFrames);
    assert.equal(restrictedResults.length, 1, JSON.stringify(restrictedFrames));
    assert.equal(restrictedResults[0].status, "failed", JSON.stringify(restrictedResults));
    assert.match(JSON.stringify(restrictedResults), /forbidden|read capabilities/);
    assert.doesNotMatch(JSON.stringify(restrictedFrames), /ALICE_PRIVATE_HISTORY/);
    trace.push({case:"model_exec_missing_history_read", expected:"denied", results:restrictedResults});
    principal = bob;
    const ordinaryFrames = await turn(bobThread, `ADMIN_TOOL read ${aliceThreads[0]}`);
    assert.equal(toolResults(ordinaryFrames).length, 0, JSON.stringify(ordinaryFrames));
    assert.match(JSON.stringify(ordinaryFrames), /TOOL_NOT_AVAILABLE|not available|not a function/);
    assert.doesNotMatch(JSON.stringify(ordinaryFrames), /ALICE_PRIVATE_HISTORY/);
    trace.push({case:"model_exec_ordinary_user", expected:"tool unavailable"});
    principal = alice;
    await call(`/v1/agents/${aliceThreads[1]}`, "DELETE", undefined, 204);
    principal = admin;
    await inspect("read", { thread_id: aliceThreads[1] }, 404);
    await inspect("diagnostics", { thread_id: aliceThreads[1] }, 404);
    assert.ok(!(await inspect("list", { owner_id: alice })).data.some(row => row.id === aliceThreads[1]));
    console.log(JSON.stringify({ evidence: output, owners, threads, real_admin_tool_results: 5, denied_admin_tool_results: 1, ordinary_tool_unavailable: true }));
  } finally {
    for (const socket of sockets) socket.terminate();
    await writeFile(join(output, "trace.json"), JSON.stringify({ command: "pnpm --filter nanocodex-managed-service run test:admin-threads", configured, provenance, trace, wire }, null, 2) + "\n");
    await writeFile(join(output, "runtime.log"), logs.join("\n") + "\n");
    await mf.dispose();
  }
});
