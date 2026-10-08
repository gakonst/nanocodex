import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";


// Real web adapter + SDK + HTTP account proxy + managed SQLite admission.
// Only synthetic account/gated model and terminal-result setup are fixture-owned.
const source = `
import worker, { DurableAgentSession, commitManagedTransition } from "./src/index.ts";
import { DurableEventLog } from "./src/durable-events.ts";
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, attachAgent } from "./src/account-auth.ts";
import { routeManaged } from "../account/worker/managedProxy.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/__seed") {
      const b = await request.json();
      await super.fetch(new Request("https://session.internal/__initialize-fixture"));
      this.ctx.storage.sql.exec("INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active) VALUES(1,?,?,?,?,?,'https://fixture.test','managed',1,123)",b.id,b.user,b.organizationId,b.teamId,b.authorizationEpoch);
      // An earlier retained turn gates model startup. Admission and event transport remain real.
      this.ctx.storage.sql.exec("INSERT INTO managed_turns(id,request_hash,input_json,authorization_json,state,accepted_cursor,created_at,accepted_at,updated_at,retry_at) VALUES('fixture-gate','fixture','{}','{}','accepted',1,123,123,123,?)",Date.now()+3600000);
      return new Response(null,{status:204});
    }
    if (path === "/__settle") {
      const {id} = await request.json();
      commitManagedTransition(this.ctx.storage,new DurableEventLog(this.ctx.storage),id,{type:'turn_completed',id,final_message:'Sign-in remains cancelled.',usage:null,citations:[]});
      return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export default {async fetch(request,env,ctx) {
  const url=new URL(request.url);
  if(env.EDGE)return await routeManaged(request,env,url)??new Response(null,{status:404});
  if(url.pathname==='/__fixture') {
    const {user,id}=await request.json();await ensureAccount(env,user,true);
    const auth=await(await env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/authorization')).json();
    if(id){await attachAgent(env,user,id);await env.NANOCODEX_SESSIONS.getByName(id).fetch('https://session.internal/__seed',{method:'POST',body:JSON.stringify({user,id,...auth.grant})});}
    return Response.json(await createApiKey(env,{kind:'api_key',userId:user,...auth.grant,subjectId:'fixture:'+user,credentialId:'fixture'},'synthetic-receipt'));
  }
  if(url.pathname==='/__settle')return env.NANOCODEX_SESSIONS.getByName(url.searchParams.get('agent')).fetch(request);
  return worker.fetch(request,env,ctx);
}};
`;
test("cancelled browser login receipt is admitted once across remounts and restart", {timeout:120_000}, async()=>{
  const root = fileURLToPath(new URL("..", import.meta.url));
  const output = fileURLToPath(new URL("../../../output", import.meta.url));
  const persistence = output + "/browser-receipt-store-" + crypto.randomUUID();
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

  await mkdir(output,{recursive:true});
  const clientBundle = output + '/browser-receipt-client.mjs';
  await build({stdin:{contents:'export { managedTerminalAgent } from "../account/src/managedAgentRuntime.ts"; export { Agent } from "nanocodex/managed";',resolveDir:root},bundle:true,format:'esm',platform:'node',outfile:clientBundle});
  const {managedTerminalAgent,Agent}=await import(clientBundle+'?run='+crypto.randomUUID());
  let mf=new Miniflare(options);const trace=[];
  try {
    let backend=await mf.getWorker('managed'),base=await mf.ready;
    const owner=crypto.randomUUID(),id='11111111-1111-7111-8111-111111111111';
    const fixture=await backend.fetch('https://fixture.test/__fixture',{method:'POST',body:JSON.stringify({user:owner,id})});
    const {token}=await fixture.json();
    const input=JSON.stringify({type:'browser_login_receipt',status:'cancelled',request_id:'22222222-2222-4222-8222-222222222222'});
    const requests=[];
    const connect=()=>Agent.open(id,{apiKey:token,baseUrl:base.origin,fetch:async(url,init)=>{
      const r=await fetch(url,init);if(init?.method==='POST'&&String(url).endsWith('/turns'))requests.push({status:r.status,body:await r.clone().json()});return r;
    }});
    const deliver=async(text=input)=>{
      const agent=managedTerminalAgent(connect());const turn=agent.turn.prompt({input:text});
      const before=requests.length;const pending=turn.result().catch(()=>{});
      for(let n=0;requests.length===before&&n<200;n++)await new Promise(r=>setTimeout(r,10));
      assert.ok(requests.length>before,'actual receipt POST must settle');
      turn.dispose();await pending;return requests.at(-1);
    };
    const first=await deliver();assert.equal(first.status,202,JSON.stringify(first));const turnId=first.body.turn_id;
    const second=await deliver();assert.equal(second.status,200,JSON.stringify(second));assert.equal(second.body.turn_id,turnId);
    const settled=await backend.fetch('https://fixture.test/__settle?agent='+id,{method:'POST',body:JSON.stringify({id:turnId})});assert.equal(settled.status,204);
    await mf.dispose();mf=new Miniflare(options);backend=await mf.getWorker('managed');base=await mf.ready;
    const third=await deliver();assert.equal(third.status,200);assert.equal(third.body.turn_id,turnId);assert.equal(third.body.state,'completed');
    const native=await fetch(new URL('/v1/agents/'+id+'/turns',base),{method:'POST',headers:{authorization:'Bearer '+token,'content-type':'application/json'},body:JSON.stringify({id:'browser-login-22222222-2222-4222-8222-222222222222-cancelled',input})});
    assert.equal(native.status,200);assert.equal((await native.json()).turn_id,turnId);
    // Normal messages still create distinct turns, even when their text repeats.
    const normal1=await deliver('Keep sign-in cancelled.');const normal2=await deliver('Keep sign-in cancelled.');assert.notEqual(normal1.body.turn_id,normal2.body.turn_id);
    const history=await connect().events.page({limit:100});
    const accepted=history.data.filter(e=>e.data.type==='turn_accepted'&&e.data.input===input);
    assert.equal(accepted.length,1,'one accepted receipt after repeated delivery and restart');
    trace.push({requests,acceptedReceiptTurns:accepted.length,receipt:JSON.parse(input),signInActions:0});
  } finally {await mf.dispose();await writeFile(output+'/browser-receipt-trace.json',JSON.stringify(trace,null,2));await rm(persistence,{recursive:true,force:true});}
});
