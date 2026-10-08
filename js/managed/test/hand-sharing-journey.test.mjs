import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
const execute = promisify(execFile);
const cliKeys = { owner: "ncx_live_" + "a".repeat(12) + "_" + "b".repeat(43), recipient: "ncx_live_" + "c".repeat(12) + "_" + "d".repeat(43) };
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { EXEC_COMMAND_PARAMETERS, WRITE_STDIN_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "../../nanocodex-tools/tools/execution-contract.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "11111111-1111-4111-8111-111111111111";
const principal = { kind: "api_key", userId: owner, organizationId: "22222222-2222-4222-8222-222222222222",
  teamId: "33333333-3333-4333-8333-333333333333", role: "owner", subjectId: `user:${owner}`,
  credentialId: "synthetic-inventory", authorizationEpoch: 1,
  capabilities: ["agents:read", "agents:write", "tools:use"] };
const recipient = "44444444-4444-4444-8444-444444444444";
const source = `
import { DurableObject } from 'cloudflare:workers';
import { AccountHostedToolsProvider } from './src/account-hosted-tools.ts';
import worker, { AccountHostedTools, DurableAgentSession } from './src/index.ts';
export { AccountHostedTools, DurableAgentSession };
import { routeManaged } from '../account/worker/managedProxy.ts';
// No public direct tool-invocation endpoint exists. This narrow driver retains
// a real provider handle so revocation can be tested against a cached route;
// share management/discovery still run through the public managed worker.
export class SharingDriver extends DurableObject {
  async fetch(request) {
    const {operation,machine,call}=await request.json();
    if(operation==='bind') {
      this.provider=new AccountHostedToolsProvider(this.env.NANOCODEX_ACCOUNT_TOOLS,'${recipient}',()=>true,undefined);
      await this.provider.refresh(); this.tool=this.provider.machineTool(machine,'exec_command');
      return Response.json({bound:!!this.tool});
    }
    if(operation==='bind-screen') {
      await this.provider.refresh(); this.screen=this.provider.screenTool(machine);
      return Response.json({bound:!!this.screen});
    }
    if(operation==='end') {
      await this.provider.endTurn('sharing-test','sharing-turn','Interrupt');
      return Response.json({ended:true});
    }
    const isScreen=operation==='screen'||operation==='cancel-screen';
    const tool=isScreen?this.screen:operation==='stdin'?this.process:operation==='cua'?this.provider.machineTool(machine,'mcp__cua_repl__js'):this.tool;
    const controller=new AbortController();
    const timer=(operation==='cancel'||operation==='cancel-screen')?setTimeout(()=>controller.abort(),250):undefined;
    const result=await tool.handler(isScreen?{action:'observe'}:operation==='stdin'?{session_id:7,chars:'hello'}:operation==='cua'?{code:'observe'}:{cmd:operation==='cancel'?'HOLD':'printf SHARED_OK',workdir:'/synthetic/workspace'},
      {sessionId:'sharing-test',turnId:'sharing-turn',callId:call,model:'synthetic',signal:(operation==='cancel'||operation==='cancel-screen')?controller.signal:request.signal});
    if(timer) clearTimeout(timer);
    if(operation==='invoke' && result[Symbol.for('nanocodex.processSessionTool')]) this.process=result[Symbol.for('nanocodex.processSessionTool')];
    return Response.json(result);
  }
}
export default {async fetch(request,env,ctx) {
  // Only external authentication is substituted; all share/Hand routes are shipped code.
  const auth=request.headers.get('authorization');
  const role=Object.entries(${JSON.stringify(cliKeys)}).find(([,key])=>auth==='Bearer '+key)?.[0] ?? auth?.replace('Bearer synthetic-','');
  if(!['owner','recipient','reader','tools','none','connect','browser'].includes(role)) return Response.json({error:'unauthorized'},{status:401});
  if(new URL(request.url).pathname==='/__fixture/driver' && role==='recipient') return env.DRIVER.getByName('sharing-driver').fetch(request);
  const actor=${JSON.stringify(principal)};
  if(role==='recipient') actor.userId='${recipient}';
  if(role==='reader') actor.capabilities=['agents:read'];
  if(role==='tools') actor.capabilities=['tools:use'];
  if(role==='none') actor.capabilities=[];
  if(role==='connect') actor.connectGrant={grantId:'synthetic-grant'};
  if(role==='browser') actor.kind='account_session';
  return await routeManaged(request, { NANOCODEX_BACKEND: {
    fetch: forwarded => worker.fetch(new Request(forwarded, {
      cf: { continent: "EU", country: "DE", longitude: "8.68" },
    }),env,ctx,actor),
  } }, new URL(request.url)) ?? Response.json({error:'not_found'}, {status:404});
}};
`;

for (const regional of [false, true]) test(`revocable account Hand sharing (${regional ? "versioned" : "legacy"})`, { timeout: 60_000 }, async () => {
  const output = join(repo, "output/hand-sharing-journey", `${Date.now()}-${process.pid}-${regional ? "regional" : "legacy"}`);
  await mkdir(output, { recursive: true });
  const http = [], wire = [], sockets = [], assets = [], cli = [];
  let wasmSequence = 0;
  let mf, base, failure;
  const request = async (path, options = {}) => {
    const started = performance.now();
    const response = await fetch(new URL(path, base), { headers: { authorization: "Bearer synthetic-owner" },
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
      { headers: { authorization: "Bearer synthetic-owner", ...(versioned ? {
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
    const catalog = { type: "catalog", attachment_id: id, ...(versioned ? { runtime_id: runtime } : {}), turn_lifecycle: true, capabilities: ["turn_metadata"],
      machines: [{ id, name: id, workspace: "/synthetic/workspace", capabilities: ["native"] }],
      tools: [{ provider: "native", remote_name: "exec_command", parallel_safe: true, timeout_ms: 15_000,
        definition: { type: "function", name: "exec_command", description: "Synthetic device", strict: false,
          parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA } },
        { provider: "native", remote_name: "write_stdin", parallel_safe:true, timeout_ms:15000,
          definition:{type:"function",name:"write_stdin",description:"Synthetic process",strict:false,
            parameters:WRITE_STDIN_PARAMETERS,output_schema:EXECUTION_OUTPUT_SCHEMA}},
        { provider:"native",remote_name:"mcp__cua_repl__js",parallel_safe:false,timeout_ms:15000,
          definition:{type:"function",name:"mcp__cua_repl__js",description:"Synthetic CUA",strict:false,
            parameters:{type:"object",properties:{code:{type:"string"}},additionalProperties:false}}}] };
    wire.push({ id, direction: "publisher", frame: catalog }); socket.send(JSON.stringify(catalog));
    assert.equal((await ready).type, "ready");
    socket.on('message',data=>{
      const frame=JSON.parse(String(data)); wire.push({id,direction:'broker',frame});
      if(frame.type==='cancel') socket.send(JSON.stringify({type:'result',call_id:frame.call_id,outcome:{status:'cancelled',message:'fixture cancelled'}}));
      if(frame.type==='call' && frame.input?.cmd!=='HOLD') {
        const result={type:'result',call_id:frame.call_id,outcome:{status:'completed',output:{
          output:'SHARED_OK',success:true,structured_result:{output:'SHARED_OK'},metadata:null,process_trace:null}}};
        wire.push({id,direction:'publisher',frame:result}); socket.send(JSON.stringify(result));
      }
    });
    return socket;
  }
  const inventory = () => request("/v1/account/hands/inventory");
  const start = async bundle => {
    mf = new Miniflare({ port: 0, durableObjectsPersist: join(output, "sqlite"),
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle }, ...assets],
      durableObjects: { DRIVER: {className:"SharingDriver",useSQLite:true}, NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
        NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true } },
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
    const as = actor => ({ authorization: `Bearer synthetic-${actor}` });
    const post = (path, body, actor = 'owner', headers = {}) => request(path, {
      method: 'POST', headers: { ...as(actor), 'content-type': 'application/json', ...headers }, body: JSON.stringify(body),
    });
    const shares = '/v1/account/hand-shares';
    const recipientInventory = () => request('/v1/account/hands/inventory', {headers: as('recipient')});
    assert.deepEqual((await request(shares)).value, {data: []});
    assert.deepEqual((await recipientInventory()).value.data, []);
    await publish('/v1/account/tool-host', 'owner-device');
    await publish('/v1/account/tool-host', 'private-device');
    for (const [path, method, body] of [
      [shares, 'GET'], [shares, 'POST', {machine_id:'owner-device'}],
      [shares+'/redeem', 'POST', {url:'https://synthetic.invalid/hand-share/'+owner+'#token=nhs_'+'a'.repeat(43)}],
      [shares+'/55555555-5555-4555-8555-555555555555', 'DELETE'],
    ]) {
      const init = {method, ...(body ? {body:JSON.stringify(body)} : {})};
      assert.equal((await request(path,{...init,headers:{}})).status,401);
      for(const actor of ['reader','tools','none','connect']) {
        assert.equal((await request(path,{...init,headers:{...as(actor),'content-type':'application/json'}})).status,403, actor+' '+method+' '+path);
      }
    }
    for(const headers of [{}, {origin:'https://cross-origin.invalid'}]) {
      assert.equal((await post(shares,{machine_id:'owner-device'},'browser',headers)).status,403);
      assert.equal((await request(shares+'/55555555-5555-4555-8555-555555555555',{method:'DELETE',headers:{...as('browser'),...headers}})).status,403);
      assert.equal((await post(shares+'/redeem',{url:'invalid'},'browser',headers)).status,403);
    }
    assert.equal((await post(shares,{machine_id:'owner-device'},'recipient')).status,404);
    assert.equal((await post(shares,{machine_id:'absent'})).status,404);
    const created=await post(shares,{machine_id:'owner-device'},'browser',{origin:base.origin});
    assert.equal(created.status,201,JSON.stringify(created));
    const share=created.value;
    assert.equal(share.machine_id,'owner-device');
    assert.equal(typeof share.id,'string');
    const link=new URL(share.url);
    assert.equal(link.pathname,'/hand-share/'+owner);
    assert.match(link.hash,/^#token=nhs_[A-Za-z0-9_-]{43}$/);
    assert.equal(link.search,'');
    for(const invalid of [share.url.replace(link.origin,'https://foreign.invalid'),share.url.replace('#token=','#other='),share.url+'x']) {
      assert.equal((await post(shares+'/redeem',{url:invalid},'recipient')).status,400,'malformed or foreign-origin link rejected');
    }
    assert.equal((await post(shares+'/redeem',{url:share.url.slice(0,-1)+(share.url.endsWith('a')?'b':'a')},'recipient')).status,404,'unknown secret rejected');
    const listed=await request(shares);
    assert.equal(http.at(-1).cacheControl,'no-store');
    assert.equal(listed.value.data.length,1);
    assert.equal(listed.value.data[0].id,share.id);
    assert.ok(!JSON.stringify(listed.value).includes(link.hash.slice(7)),'list never discloses bearer token');
    assert.deepEqual((await request(shares,{headers:as('recipient')})).value,{data:[]});
    assert.equal((await request(shares+'/'+share.id,{method:'DELETE',headers:as('recipient')})).status,404);
    assert.deepEqual((await recipientInventory()).value.data,[]);
    const redeemed=await post(shares+'/redeem',{url:share.url},'recipient');
    assert.equal(redeemed.status,200,JSON.stringify(redeemed));
    const alias=redeemed.value.machine_id;
    assert.equal(typeof alias,'string');
    assert.notEqual(alias,'owner-device');
    assert.deepEqual(await post(shares+'/redeem',{url:share.url},'recipient'),redeemed,'redemption is idempotent');
    const discovered=await recipientInventory();
    assert.equal(discovered.value.data.length,1,'unshared sibling stays private');
    assert.equal(discovered.value.data[0].id,alias);
    assert.equal(discovered.value.data[0].online,true);
    assert.equal((await post(shares,{machine_id:alias},'recipient')).status,404,'recipient cannot reshare foreign machine');
    assert.deepEqual((await post('/__fixture/driver',{operation:'bind',machine:alias},'recipient')).value,{bound:true});
    const invoked=await post('/__fixture/driver',{operation:'invoke',call:'before-revoke'},'recipient');
    assert.equal(invoked.value.structuredResult.output,'SHARED_OK',JSON.stringify(invoked));
    const calls=()=>wire.filter(row=>row.direction==='broker' && row.frame.type==='call');
    assert.equal(calls().length,1);
    assert.equal(calls()[0].id,'owner-device');
    assert.equal((await post('/__fixture/driver',{operation:'stdin',call:'process-before-revoke'},'recipient')).value.success,true);
    assert.equal((await post('/__fixture/driver',{operation:'cua',machine:alias,call:'cua-before-revoke'},'recipient')).value.success,true);
    assert.equal(calls().length,3);
    const cancelFrame=new Promise((resolve,reject)=> {
      const timer=setTimeout(()=>reject(new Error('cancellation frame not delivered')),3000);
      for(const socket of sockets) socket.on('message',data=> { if(JSON.parse(String(data)).type==='cancel') { clearTimeout(timer);resolve(); } });
    });
    const cancelled=await post('/__fixture/driver',{operation:'cancel',call:'cancelled-call'},'recipient');
    assert.equal(cancelled.value.success,false);
    await cancelFrame;
    assert.ok(wire.some(row=>row.direction==='broker' && row.frame.type==='cancel'),'cancellation reaches publisher');
    assert.equal(calls().length,4);
    const cleanupFrame=new Promise((resolve,reject)=> {
      const timer=setTimeout(()=>reject(new Error('turn cleanup frame not delivered')),3000);
      for(const socket of sockets) socket.on('message',data=> {
        if(JSON.parse(String(data)).type==='turn_ended') {clearTimeout(timer);resolve();}
      });
    });
    assert.equal((await post('/__fixture/driver',{operation:'end'},'recipient')).value.ended,true);
    await cleanupFrame;
    const cleanup=wire.filter(row=>row.direction==='broker' && row.frame.type==='turn_ended');
    assert.equal(cleanup.length,1,JSON.stringify(cleanup));
    assert.equal(cleanup[0].id,'owner-device');
    assert.equal(cleanup[0].frame.session_id,'shared:'+createHash('sha256').update(JSON.stringify([recipient,'sharing-test'])).digest('hex'));
    assert.equal(cleanup[0].frame.hook_event_name,'Interrupt');
    assert.deepEqual(await request(shares+'/'+share.id,{method:'DELETE'}),{status:200,value:{revoked:true}});
    assert.deepEqual((await recipientInventory()).value.data,[],'revocation immediately removes discovery');
    assert.ok([403,404,410].includes((await post(shares+'/redeem',{url:share.url},'recipient')).status),'revoked token cannot be redeemed');
    const denied=await post('/__fixture/driver',{operation:'invoke',call:'after-revoke'},'recipient');
    assert.equal(denied.value.success,false,JSON.stringify(denied));
    assert.equal(calls().length,4,'cached route cannot dispatch after revocation');
    const replay=await post('/__fixture/driver',{operation:'invoke',call:'before-revoke'},'recipient');
    assert.equal(replay.value.success,false,'revocation also rejects replay of a previously admitted route');
    assert.equal(calls().length,4);
    assert.equal((await post('/__fixture/driver',{operation:'stdin',call:'process-after-revoke'},'recipient')).value.success,false);
    assert.equal(calls().length,4,'saved process route rejected after revoke');
    assert.equal((await inventory()).value.data.length,2,'revocation preserves owner machines');
    // A native screen publisher exercises the separate screen relay path (not a CUA MCP tool).
    const screenSocket = new WebSocket(new URL('/v1/account/hands/host',base).href.replace(/^http/,'ws'),
      {headers:as('owner')});
    sockets.push(screenSocket);
    let answerScreen = true;
    await new Promise((resolve,reject)=> {
      const timer=setTimeout(()=>reject(new Error('screen publication timed out')),5000);
      screenSocket.once('error',reject);
      screenSocket.on('message',data=> {
        const frame=JSON.parse(String(data)); wire.push({id:'screen-device',direction:'broker',frame});
        if(frame.type==='ready') {
          screenSocket.send(JSON.stringify({type:'catalog',machine_id:'screen-device',machine_name:'Shared screen',
            surfaces:[{id:'desktop',name:'Desktop',kind:'desktop',width:1,height:1,controllable:true,agent_tools:true}]}));
        }
        if(frame.type==='published') {clearTimeout(timer);resolve();}
        if(frame.type==='agent_call' && answerScreen) screenSocket.send(JSON.stringify({type:'agent_result',
          request_id:frame.request_id,status:'ok',jpeg:'/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAEBAQ==',width:1,height:1}));
      });
    });
    const screenShare=await post(shares,{machine_id:'screen-device'});
    assert.equal(screenShare.status,201,JSON.stringify(screenShare));
    const screenAlias=(await post(shares+'/redeem',{url:screenShare.value.url},'recipient')).value.machine_id;
    assert.equal((await post('/__fixture/driver',{operation:'bind-screen',machine:screenAlias},'recipient')).value.bound,true);
    const observed=await post('/__fixture/driver',{operation:'screen',call:'screen-observe'},'recipient');
    assert.equal(observed.value.structuredResult?.status ?? observed.value.status,'ok',JSON.stringify(observed));
    const screenCalls=()=>wire.filter(row=>row.id==='screen-device'&&row.frame.type==='agent_call');
    assert.equal(screenCalls().length,1);
    assert.equal(screenCalls()[0].frame.agent_id,'shared:'+createHash('sha256').update(JSON.stringify([recipient,'sharing-test'])).digest('hex'));
    answerScreen=false;
    const screenCancelled=new Promise((resolve,reject)=> {
      const timer=setTimeout(()=>reject(new Error('screen cancellation not delivered')),3000);
      screenSocket.on('message',data=> {if(JSON.parse(String(data)).type==='agent_cancel'){clearTimeout(timer);resolve();}});
    });
    await post('/__fixture/driver',{operation:'cancel-screen',call:'screen-cancel'},'recipient');
    await screenCancelled;
    assert.equal(screenCalls().length,2);
    await request(shares+'/'+screenShare.value.id,{method:'DELETE'});
    const deniedScreen=await post('/__fixture/driver',{operation:'screen',call:'screen-after-revoke'},'recipient');
    assert.equal(deniedScreen.value.success,false,JSON.stringify(deniedScreen));
    assert.equal(screenCalls().length,2,'revocation blocks cached screen route');
    if (process.env.NANOCODEX_TEST_CLI) {
      const cliHome = join(output, 'cli-home'); await mkdir(cliHome);
      const runCLI = async (actor, args) => {
        const result = await execute(process.env.NANOCODEX_TEST_CLI, ['hand-share', ...args], {
          env: {PATH:process.env.PATH, HOME:cliHome, NANOCODEX_HOME:cliHome,
            NANOCODEX_MANAGED_URL:base.origin, NANOCODEX_API_KEY:cliKeys[actor]}, timeout:20_000,
        });
        const value = JSON.parse(result.stdout);
        cli.push({actor,command:args[0],value}); return value;
      };
      const cliShare = await runCLI('owner', ['create','owner-device']);
      assert.equal((await runCLI('owner',['list'])).data[0].id,cliShare.id);
      const cliReceipt = await runCLI('recipient',['redeem',cliShare.url]);
      assert.equal((await recipientInventory()).value.data[0].id,cliReceipt.machine_id);
      assert.equal((await runCLI('owner',['revoke',cliShare.id])).status,'revoked');
      assert.deepEqual((await recipientInventory()).value.data,[]);
    }
    if (!regional) {
      const capacityShares=[];
      for(let i=0;i<100;i++) {
        const made=await post(shares,{machine_id:'owner-device'});
        assert.equal(made.status,201); capacityShares.push(made.value);
        assert.equal((await post(shares+'/redeem',{url:made.value.url},'recipient')).status,200);
      }
      assert.equal((await post(shares+'/redeem',{url:capacityShares[99].url},'recipient')).status,200,'existing redemption succeeds at capacity');
      assert.equal((await request(shares+'/'+capacityShares[0].id,{method:'DELETE'})).status,200);
      const replacement=await post(shares,{machine_id:'owner-device'});
      assert.equal(replacement.status,201);
      assert.equal((await post(shares+'/redeem',{url:replacement.value.url},'recipient')).status,200,'revoked references release capacity');
    }
  } catch (error) { failure = error; throw error; }
  finally {
    for (const socket of sockets) socket.terminate();
    await mf?.dispose();
    await writeFile(join(output, "evidence.json"), JSON.stringify({
      command: "pnpm --filter nanocodex-managed-service exec node --test test/hand-sharing-journey.test.mjs", http, wire, cli,
      expected: "owner-isolated share links; authenticated scoped admission; browser CSRF; idempotent redeem; recipient alias discovery; immediate revocation", regional,
      executionBoundary: "real AccountHostedToolsProvider retained handler via narrow fixture driver; public agent model boundary not exercised",
      passed: !failure, error: failure?.stack }, null, 2));
    console.log(`Hand sharing evidence: ${output}`);
  }
});
