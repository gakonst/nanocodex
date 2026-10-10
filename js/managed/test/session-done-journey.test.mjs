import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

// Actual HTTP -> shipped account proxy -> shipped managed router/auth -> SQLite
// session + account registry. Fixture-only routes create synthetic retained data;
// no provider, real account, production deployment, or model call is involved.
const source = `
import worker, { DurableAgentSession } from "./src/index.ts";
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, attachAgent } from "./src/account-auth.ts";
import { AgentPresentationWriter } from "./src/agent-presentation.ts";
import { routeManaged } from "../account/worker/managedProxy.ts";
import { Kv } from "accounts/server";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export class FixtureSession extends DurableAgentSession {
  #users = this.env.NANOCODEX_USERS;
  #initialized = false;
  async fetch(request) {
    const path = new URL(request.url).pathname;
    // A fresh Session creates its schema on its first real request, not in its
    // constructor. Fixture seeding and inspection use that same entry first.
    if (path.startsWith("/__") && !this.#initialized) {
      this.#initialized = true;
      await super.fetch(new Request(new URL("/__fixture-initialize", request.url)));
    }
    if (path === "/__delivery") {
      const { fail } = await request.json(); const users = this.#users;
      Object.defineProperty(this, "env", { configurable: true, value: { ...this.env, NANOCODEX_USERS: {
        getByName: (...args) => ({ fetch: async (input, init) => fail && new URL(typeof input === 'string' ? input : input.url).pathname.endsWith('/presentation')
          ? new Response(null,{status:503}) : users.getByName(...args).fetch(input, init) }),
      } } });
      return new Response(null,{status:204});
    }
    if (path === "/__delivery-state") return Response.json({
      alarm: await this.ctx.storage.getAlarm(),
      row: this.ctx.storage.sql.exec("SELECT value,delivered_revision FROM agent_presentation").toArray()[0],
    });
    if (path === "/__seed") {
      const b = await request.json();
      this.ctx.storage.sql.exec("INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active) VALUES(1,?,?,?,?,?,'https://fixture.test','managed',1,123)",b.id,b.user,b.organizationId,b.teamId,b.authorizationEpoch);
      this.ctx.storage.sql.exec("INSERT INTO managed_turns(id,request_hash,input_json,authorization_json,state,accepted_cursor,created_at,accepted_at,updated_at,retry_at) VALUES('fixture-turn','fixture','{}','{}','accepted',1,123,123,123,?)", Date.now()+3600000);
      if (b.legacy) return Response.json(null);
      const value = {revision:1,status:"running",activeTurnIds:["fixture-turn"],updatedAt:123,lastUserMessageAt:123,lastUserPrompt:"Synthetic request"};
      this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS agent_presentation(singleton INTEGER PRIMARY KEY CHECK(singleton=1),value TEXT NOT NULL,delivered_revision INTEGER NOT NULL DEFAULT 0)");
      this.ctx.storage.sql.exec("INSERT INTO agent_presentation(singleton,value,delivered_revision) VALUES(1,?,1)",JSON.stringify(value));
      return Response.json(value);
    }
    if (path === "/__inspect") return Response.json({session:this.ctx.storage.sql.exec("SELECT accepted_turns,last_active FROM session_state").toArray(),turns:this.ctx.storage.sql.exec("SELECT id,state,updated_at FROM managed_turns").toArray(),cancel:this.ctx.storage.sql.exec("SELECT * FROM managed_turn_cancel_intents").toArray()});
    if (path === "/__complete") {
      const { user } = await request.json();
      this.ctx.storage.sql.exec("UPDATE managed_turns SET state='completed',retry_at=NULL WHERE id='fixture-turn'");
      const writer = new AgentPresentationWriter(this.ctx.storage, async value => {
        const r = await this.env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/agents/'+new URL(request.url).searchParams.get('id')+'/presentation',{method:'POST',body:JSON.stringify(value)});
        if (!r.ok) throw new Error('presentation publish failed');
      },async()=>undefined,p=>this.ctx.waitUntil(p));
      writer.observe('completed',[],''); await writer.flush(); return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export default { async fetch(request, env, ctx) {
  const url = new URL(request.url);
  if (env.EDGE) return await routeManaged(request,env,url) ?? new Response('not_found',{status:404});
  if (url.pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    if (b.id) {
      await attachAgent(env,b.user,b.id);
      const p = await (await env.NANOCODEX_SESSIONS.getByName(b.id).fetch('https://session.internal/__seed',{method:'POST',body:JSON.stringify({...b,...auth.grant})})).json();
      await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/agents/'+b.id+'/activity',{method:'POST',body:JSON.stringify({title:'Synthetic session',turnCount:1})});
      if (p) await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/agents/'+b.id+'/presentation',{method:'POST',body:JSON.stringify(p)});
    }
    if (b.session) {
      const token = 's_'+ 'D'.repeat(43);
      await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,{userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
      return Response.json({cookie:'nanocodex_account='+token});
    }
    return Response.json(await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,credentialId:'fixture',capabilities:b.readOnly?['agents:read']:auth.grant.capabilities},'synthetic-done-journey'));
  }
  if (['/__inspect','/__complete','/__delivery','/__delivery-state'].includes(url.pathname)) return env.NANOCODEX_SESSIONS.getByName(url.searchParams.get('id')).fetch(request);
  return worker.fetch(request,env,ctx);
}};
`;

test("manual Done/Undone survives status changes and restart without cancelling a session", { timeout: 120_000 }, async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const output = fileURLToPath(new URL("../../../output", import.meta.url));
  const persistence = output + "/session-done-store-" + crypto.randomUUID();
  const wasm = [];
  let wasmIndex = 0;
  const bundled = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false, format: "esm", target: "es2022", platform: "node", banner: { js: 'import { createRequire } from "node:module"; const require = createRequire("/worker.mjs");' }, external: ["cloudflare:*", "node:*"], alias: { "node-rsa": root + "/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" }, plugins: [{ name: "wasm-modules", setup(build) {
    build.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = fileURLToPath(new URL(args.path, 'file://' + args.resolveDir + '/'));
      const name = './fixture-' + wasmIndex++ + '.wasm';
      wasm.push({ type: 'CompiledWasm', path: name, contents: await readFile(path) });
      return { path: name, external: true };
    });
  } }] });
  const modules = [{ type: "ESModule", path: "worker.mjs", contents: bundled.outputFiles[0].text }, ...wasm];
  const options = { durableObjectsPersist: persistence, workers: [
    { name: "edge", modules, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"], bindings: { EDGE: true }, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { name: "managed", modules, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat", "enable_request_signal"], durableObjects: {
      NANOCODEX_SESSIONS: { className: "FixtureSession", useSQLite: true }, NANOCODEX_USERS: { className: "UserAccount", useSQLite: true }, NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true }, NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true }, NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
    } },
  ] };
  let mf = new Miniflare(options);
  const trace = [];
  try {
    let backend = await mf.getWorker('managed'), base = await mf.ready;
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    async function fixture(body) { const r = await backend.fetch('https://fixture.test/__fixture',{method:'POST',body:JSON.stringify(body)}); assert.equal(r.status,200,await r.clone().text()); return r.json(); }
    const { token } = await fixture({user:owner,id});
    const other = (await fixture({user:crypto.randomUUID()})).token;
    const readOnly = (await fixture({user:owner,readOnly:true})).token;
    const { cookie } = await fixture({user:owner,session:true});
    async function call(path,method='GET',body,credential=token,expected=200,headers={}) {
      const r = await fetch(new URL(path,base),{method,headers:{...(credential?{authorization:'Bearer '+credential}:{}),'content-type':'application/json',...headers},...(body===undefined?{}:{body:typeof body==='string'?body:JSON.stringify(body)})});
      const text = await r.text(); let data;try {data=JSON.parse(text);}catch{data=text;}
      trace.push({path,method,status:r.status,data});assert.equal(r.status,expected,method+' '+path+': '+text);return data;
    }
    const path = '/v1/agents/'+id+'/done';
    const summary = async()=> (await call('/v1/agents')).summaries[id];
    const inspect = async()=> (await backend.fetch('https://fixture.test/__inspect?id='+id)).json();
    const before = await inspect();
    const legacyId = crypto.randomUUID();await fixture({user:owner,id:legacyId,legacy:true});
    const legacyBefore = (await call('/v1/agents')).summaries[legacyId];
    assert.ok(legacyBefore.last_user_message_at>0);
    await call('/v1/agents/'+legacyId+'/done','PUT',{done:true});
    assert.equal((await call('/v1/agents')).summaries[legacyId].last_user_message_at,legacyBefore.last_user_message_at);
    assert.equal((await summary()).presentation.done,false);
    assert.equal((await summary()).presentation.doneAt,null);
    for (const [key,status] of [[null,401],[other,404],[readOnly,403]]) await call(path,'PUT',{done:true},key,status);
    await call('/v1/agents/'+crypto.randomUUID()+'/done','PUT',{done:true},token,404);
    await call(path,'PUT',{done:true},null,403,{cookie,origin:'https://unrelated.test'});
    await call(path,'PUT',{done:false},null,200,{cookie,origin:base.origin});
    await call(path+'?unexpected=1','PUT',{done:true},token,400);
    for (const body of [{done:'true'}, {}, null, 'not-json']) await call(path,'PUT',body,token,400);
    await call(path,'POST',{done:true},token,405);
    const done = await call(path,'PUT',{done:true});assert.equal(done.done,true);assert.ok(done.done_at>0);
    assert.deepEqual(await call(path,'PUT',{done:true}),done);
    let s = await summary();assert.equal(s.presentation.doneAt,done.done_at);assert.equal(s.presentation.status,'running');assert.equal(s.last_user_message_at,123);
    assert.deepEqual(await inspect(),before);
    // Complete synthetic retained runtime data while the Worker is cold so the
    // presentation writer reconstructs its durable value, as after a restart.
    await mf.dispose();mf = new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    assert.deepEqual(await call(path,'PUT',{done:true}),done);
    await mf.dispose();mf = new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    const complete = await backend.fetch('https://fixture.test/__complete?id='+id,{method:'POST',body:JSON.stringify({user:owner})});assert.equal(complete.status,204);
    s=await summary();assert.equal(s.presentation.done,true);assert.equal(s.presentation.doneAt,done.done_at);assert.equal(s.presentation.status,'completed');assert.equal(s.last_user_message_at,123);
    await mf.dispose();mf = new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    const undone = await call(path,'PUT',{done:false});assert.equal(undone.done,false);assert.equal(undone.done_at,null);assert.ok(undone.presentation_revision>done.presentation_revision);
    assert.deepEqual(await call(path,'PUT',{done:false}),undone);
    s=await summary();assert.equal(s.presentation.done,false);assert.equal(s.presentation.doneAt,null);assert.equal(s.presentation.status,'completed');assert.equal(s.last_user_message_at,123);
    await mf.dispose();mf = new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    assert.equal((await summary()).presentation.done,false);
    const retained = await inspect();assert.equal(retained.turns[0].id,'fixture-turn');assert.equal(retained.turns[0].state,'completed');assert.deepEqual(retained.cancel,[]);
    // A saved manual mutation is not acknowledged as visible while account
    // delivery is unavailable. Idle-session alarm retries its persisted outbox,
    // without the caller automatically resending the PUT.
    let r = await backend.fetch('https://fixture.test/__delivery?id='+id,{method:'POST',body:JSON.stringify({fail:true})});assert.equal(r.status,204);
    await call(path,'PUT',{done:true},token,503);
    assert.equal((await summary()).presentation.done,false);
    const delivery = await (await backend.fetch('https://fixture.test/__delivery-state?id='+id)).json();
    const saved = JSON.parse(delivery.row.value);assert.equal(saved.done,true);assert.ok(saved.revision>delivery.row.delivered_revision);assert.ok(delivery.alarm>Date.now());
    // Restart ends the fixture infrastructure outage, retaining only durable
    // session data and the original alarm deadline (not an in-memory retry).
    await mf.dispose();mf = new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    await new Promise(resolve=>setTimeout(resolve,21000));
    const retried = await summary();assert.equal(retried.presentation.done,true);assert.equal(retried.presentation.revision,saved.revision);assert.equal(retried.presentation.doneAt,saved.doneAt);assert.equal(retried.last_user_message_at,123);
    await mf.dispose();mf = new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    assert.equal((await summary()).presentation.done,true);
    await call('/v1/agents/'+id,'DELETE',undefined,token,503);
    await call(path,'PUT',{done:true},token,404);
    assert.equal((await call('/v1/agents')).summaries[id],undefined);
  } finally {
    await mkdir(output,{recursive:true});await writeFile(output+'/session-done-http-journey.json',JSON.stringify({trace},null,2));
    await mf.dispose();await rm(persistence,{recursive:true,force:true});
  }
});
