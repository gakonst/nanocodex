import assert from "node:assert/strict";
import { createHash } from "node:crypto";
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

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000051";
const thread = "00000000-0000-7000-8000-000000000052";
const organization = "00000000-0000-7000-8000-000000000053";
const team = "00000000-0000-7000-8000-000000000054";
const machine = "synthetic-preparation-hand";
const localMachine = "synthetic-session-hand";
const command = "pnpm --filter nanocodex-managed-service test:hand-preparation";
const trials = Array.from({ length: 7 }, (_, i) => `MEASURE_${i}`);
const scripts = Object.fromEntries(trials.map(scenario => [scenario, `
  const results = await Promise.all(["a","b"].map(part => tools.exec_command({cmd:"printf '${scenario}:"+part+"\\n' >> effect.log; printf PREP_OK",workdir:"/${machine}",shell:"/bin/sh",login:false,yield_time_ms:1000})));
  text(results);
`]));
scripts.LOCAL = `text(await tools.exec_command({cmd:"printf LOCAL_INDEPENDENT",workdir:"/${localMachine}",shell:"/bin/sh",login:false}));`;
scripts.MIXED = `
  text(await tools.exec_command({cmd:"printf LOCAL_FIRST",workdir:"/${localMachine}",shell:"/bin/sh",login:false}));
  text(await tools.exec_command({cmd:"printf ACCOUNT_SECOND",workdir:"/${machine}",shell:"/bin/sh",login:false}));
  text(await tools.exec_command({cmd:"printf LOCAL_AGAIN",workdir:"/${localMachine}",shell:"/bin/sh",login:false}));
  try { await tools.exec_command({cmd:"printf MUST_NOT_RUN",workdir:"/unknown-hand"}); } catch(error) { text({unknown:error.message}); }
`;
scripts.RECONNECT = `
  text(await tools.exec_command({cmd:"printf PINNED_LOCAL",workdir:"/${localMachine}",shell:"/bin/sh",login:false}));
  text(await tools.exec_command({cmd:"printf DISCOVERED_ACCOUNT",workdir:"/${machine}",shell:"/bin/sh",login:false}));
  try { const result=await tools.exec_command({cmd:"printf WRONG_GENERATION",workdir:"/${localMachine}",shell:"/bin/sh",login:false}); text(result); }
  catch(error) { text({stale_route:error.message}); }
`;
scripts.RECOVER = scripts.LOCAL;
scripts.OFFLINE = `try { text(await tools.exec_command({cmd:"printf MUST_NOT_RUN_OFFLINE",workdir:"/${machine}",shell:"/bin/sh",login:false})); } catch(error) { text({offline:error.message}); }`;
scripts.PROCESS = `
  const started=await tools.exec_command({cmd:"sleep 0.3; printf PROCESS_RESUMED",workdir:"/${localMachine}",shell:"/bin/sh",login:false,yield_time_ms:1});
  if(!started.session_id) throw Error("expected retained native process");
  text(await tools.write_stdin({session_id:started.session_id,yield_time_ms:1000}));
`;
scripts.INVENTORY = `
  text(await tools.exec_command({cmd:"printf BEFORE_INVENTORY",workdir:"/${localMachine}",shell:"/bin/sh",login:false}));
  const env=await tools.environment({});text({inventory:Object.keys(env.hands)});
`;
for (let i=0;i<7;i++) scripts[`DIRECT_MEASURE_${i}`] = scripts.LOCAL;
scripts.OMITTED = 'text(await tools.exec_command({cmd:"printf OMITTED_BRAIN"}));';
scripts.EXCLUDED = 'for(const workdir of ["/slow-vm","/fast-vm","/unknown-hand"]) {try {text(await tools.exec_command({cmd:"printf UNSAFE_DISPATCH",workdir}));} catch(error) {text({excluded:workdir,error:error.message});}}';
scripts.BRAIN = 'text(await tools.exec_command({cmd:"printf BRAIN_INDEPENDENT",workdir:"/brain"}));';
scripts.FAIL = `try { text(await tools.exec_command({cmd:"printf MUST_NOT_RUN",workdir:"/${machine}"})); } catch(error) { text({expected_failure:error.message}); }`;
const mounts = ["slow", "fast"].map((name, i) => ({ id: `fixture-${name}`, root: `/${name}-vm`, provider: "host", name,
  configuration: { vm_factory_name: "fixture-factory", vm_host: { pool_locator: (i ? "b" : "a").repeat(43),
    allocation_id: `00000000-0000-7000-8000-00000000006${i}`, generation: 1, machine_id: `fixture-${name}-machine` } } }));

// Only account seed/admission, the external model and unavailable external VM
// pool receipts are fixtures. Discovery, preparation, Code Mode, SQLite ledger,
// routing, reverse WebSocket, ToolRouter and native shell are shipped code.
// Explicit artificial delays exercise dependency overlap, NOT deployed latency.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class ObservedAccountHostedTools extends AccountHostedTools {
  async fetch(request) {
    const path=new URL(request.url).pathname;
    if(path==='/__fixture') { await this.ctx.storage.put('fixture',await request.json()); return new Response(null,{status:204}); }
    if(path==='/snapshot') {
      const config=await this.ctx.storage.get('fixture')??{},started=Date.now(),selected=(await request.clone().json()).machine_id;
      console.info({type:'fixture.snapshot',stage:'start',at:started,selected:selected??null});
      await new Promise(resolve=>setTimeout(resolve,selected?0:(config.delay_ms??0)));
      for(let held;(held=await this.ctx.storage.get('fixture'))?.hold||(selected&&held?.hold_selected===selected);) await new Promise(resolve=>setTimeout(resolve,10));
      const response=config.fail?new Response('fixture discovery failure',{status:503}):await super.fetch(request);
      console.info({type:'fixture.snapshot',stage:'end',at:Date.now(),status:response.status}); return response;
    }
    return super.fetch(request);
  }
}
export class FixturePool extends DurableObject {
  async fetch(request) {
    const input=await request.json(),slow=input.pool_locator==='${"a".repeat(43)}';
    console.info({type:'fixture.pool',stage:'start',at:Date.now(),pool:slow?'slow':'fast'});
    await new Promise(resolve=>setTimeout(resolve,slow?160:40));
    console.info({type:'fixture.pool',stage:'end',at:Date.now(),pool:slow?'slow':'fast'});
    return Response.json({error:'allocation_unavailable'},{status:503});
  }
}
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    if(new URL(request.url).pathname==='/__seed') {
      // Fresh sessions defer schema setup to their first production handler.
      await (await super.fetch(new Request('https://fixture.internal/__initialize'))).body?.cancel();
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://fixture.internal/','managed',?)",'${thread}','${owner}','${organization}','${team}',Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES(1,?)",JSON.stringify({environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      for(const mount of ${JSON.stringify(mounts)}) this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_mounts VALUES(?,?,?,?,?,?,'mounted',?,?)",mount.id,mount.provider,mount.name,mount.root,mount.id,JSON.stringify(mount.configuration),Date.now(),Date.now());
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if(request.headers.get('upgrade')!=='websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client,server]=Object.values(new WebSocketPair()); server.accept(); let scenario='',index=0;
    server.addEventListener('close',()=>server.close(1000));
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data),latest=JSON.stringify((body.input??[]).filter(item=>item.role==='user').at(-1));
      const next=Object.keys(${JSON.stringify(scripts)}).find(key=>latest?.includes('PREPARATION_'+key));
      if(next&&next!==scenario) {scenario=next;index=0;} const call=++index;
      console.info({type:'fixture.model',scenario,index:call});
      const outputs=(body.input??[]).filter(item=>item.type==='custom_tool_call_output'||item.type==='function_call_output');
      const output=call===1?[{type:'custom_tool_call',name:'exec',call_id:'call_preparation_'+scenario,input:${JSON.stringify(scripts)}[scenario]}]
        :[{type:'message',role:'assistant',content:[{type:'output_text',text:JSON.stringify(outputs.at(-1))}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_preparation_'+scenario+'_'+call,status:'completed',end_turn:call>1,output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    }); return new Response(null,{status:101,webSocket:client});
  }
}
export default {fetch(request,env) {
  const url=new URL(request.url),path=url.pathname;
  if(path.startsWith('/account-tools/')) return env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').fetch(new Request('https://account-tools.internal/'+path.slice('/account-tools/'.length),request));
  if(path==='/tool-host') return env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').fetch(new Request('https://account-tools.internal/tool-host',request));
  return env.NANOCODEX_SESSIONS.getByName('fixture-session').fetch(new Request('https://session.internal'+path.replace('/v1/agents/${thread}','')+url.search,request));
}};
`;

test("fresh shipped Code Mode cells prepare selected routes without changing dispatch safety", { timeout: 90_000 }, async () => {
  const label = process.env.NANOCODEX_BENCHMARK_LABEL ?? "local";
  assert.match(label, /^[a-zA-Z0-9_-]+$/);
  const output = join(repo, "output/hand-preparation-journey", `${label}-${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, { recursive: true });
  const records = [], wire = [], http = [], runtime = [], samples = [];
  const result = { command, label, inputs: { owner, thread, machine, scripts, mounts, snapshot_delay_ms: 120, vm_delay_ms: [160,40], warmups: 2 },
    expected: { once_only_effects: 14, concurrent_cell_capture_count: 7, unavailable_vms_excluded: true, brain_bypasses_prepare: true, discovery_failure_predispatch: true, thread_native_catalog_rejected: true, mixed_cell_routes: true, reconnect_keeps_pinned_generation: true, fresh_cell_recovers: true }, observed: {} };
  const capture = line => { runtime.push(line); const start=line.indexOf('{"type":'); if(start>=0) {try {records.push(JSON.parse(line.slice(start)));} catch {}} };
  let mf, native, tools, attachment, localAttachment, failure;
  const prepSource = process.env.NANOCODEX_NAMESPACE_PREP_SOURCE;
  try {
    const assets = [];
    const plugins = [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const path=join(args.resolveDir,args.path),contents=await readFile(path),name=`fixture-${assets.length}.wasm`;
      assert.ok(path.startsWith(repo)); assets.push({type:"CompiledWasm",path:name,contents}); return {path:`./${name}`,external:true};
    }); } }];
    if (prepSource) plugins.push({name:"preparation-source",setup(builder) {builder.onLoad({filter:/\/managed\/src\/index\.ts$/},async()=>({contents:await readFile(prepSource,"utf8"),loader:"ts",resolveDir:join(root,"src")}));}});
    const bundle = await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,metafile:true,format:"esm",platform:"node",conditions:["workerd"],target:"es2022",
      banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:["cloudflare:*","node:*"],
      alias:{"nanocodex-tools/internal/hosted-machine":join(repo,"js/nanocodex-tools/tools/hostedMachine.mjs"),"nanocodex-tools/hosted":join(repo,"js/nanocodex-tools/src/hosted/index.ts"),"node-rsa":join(root,"../nanocodex/tools/browser/unsupportedNodeRsa.mjs")},plugins,logLevel:"silent"});
    const hashes = Object.fromEntries(await Promise.all(["js/managed/src/index.ts","js/managed/src/namespace-tools.ts","js/nanocodex/pkg-web/nanocodex_bg.wasm","js/managed/src/hand-call-observation.ts","js/managed/src/account-hosted-tools.ts","js/nanocodex-tools/src/hosted/broker-core.ts","js/nanocodex-tools/tools/processOutput.mjs"].map(async path => [path,createHash("sha256").update(await readFile(path.endsWith("/index.ts")&&prepSource?prepSource:join(repo,path))).digest("hex")])));
    await writeFile(join(output,"source-resolution.json"),JSON.stringify({prepSource:prepSource??null,hashes,bundleInputs:Object.keys(bundle.metafile.inputs)},null,2));
    await writeFile(join(output,"fixture-source.mjs"),source); await writeFile(join(output,"worker.mjs"),bundle.outputFiles[0].text);
    const date="2026-07-30";
    mf = new Miniflare({port:0,handleRuntimeStdio(stdout,stderr){createInterface({input:stdout}).on("line",capture);createInterface({input:stderr}).on("line",capture);},durableObjectsPersist:join(output,"sqlite"),workers:[
      {name:"managed",compatibilityDate:date,compatibilityFlags:["nodejs_compat","enable_request_signal"],modules:[{type:"ESModule",path:"worker.mjs",contents:bundle.outputFiles[0].text},...assets],bindings:{AGENT_IDLE_TIMEOUT_MS:"60000"},
        durableObjects:{NANOCODEX_SESSIONS:{className:"FixtureSession",useSQLite:true},NANOCODEX_ACCOUNT_TOOLS:{className:"ObservedAccountHostedTools",useSQLite:true},NANOCODEX_MEMORY:{className:"FixtureModel",useSQLite:true},MODEL:{className:"FixtureModel",useSQLite:true},NANOCODEX_VM_HOST_POOLS:{className:"FixturePool",useSQLite:true}},
        serviceBindings:{NANOCODEX:"provider"},r2Buckets:["NANOCODEX_HISTORY","NANOCODEX_WORKSPACES"]},
      {name:"provider",compatibilityDate:date,modules:true,script:"export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}}",durableObjects:{MODEL:{className:"FixtureModel",scriptName:"managed",useSQLite:true}}}]});
    const base=await mf.ready;
    const headers={"x-nanocodex-owner-id":owner,"x-nanocodex-session-organization-id":organization,"x-nanocodex-session-team-id":team,"x-nanocodex-authorization-epoch":"1","x-nanocodex-capabilities":JSON.stringify(["agents:read","agents:write","tools:use"]),"content-type":"application/json"};
    const request=async(path,init={})=>{const response=await fetch(new URL(path,base),{...init,headers:{...headers,...init.headers},signal:AbortSignal.timeout(10_000)}),body=await response.text();http.push({path,status:response.status,method:init.method??"GET",body});return {status:response.status,value:body?JSON.parse(body):undefined};};
    assert.equal((await request("/__seed",{method:"POST"})).status,204);
    native=await createNodeProcessTools({workspace}); tools=await createTools({tools:native.tools});
    const endpoint=new URL("/tool-host",base);endpoint.protocol="ws:";
    attachment=createAttachment(tools,{endpoint:endpoint.href,transport:{connect(){const socket=new WebSocket(endpoint,{headers:{"x-nanocodex-owner-id":owner}}),send=socket.send.bind(socket);socket.send=(data,...args)=>{wire.push({direction:"host",frame:JSON.parse(String(data))});return send(data,...args);};socket.on("message",data=>wire.push({direction:"broker",frame:JSON.parse(String(data))}));return socket;}}},{machines:[{id:machine,name:"Synthetic Preparation Hand",workspace,capabilities:["shell"]}],attachmentId:machine});
    assert.equal((await attachment.connect()).connected,true);
    // Thread-scoped workspace Hands are retired: a native catalog on the thread
    // tool host is refused with a migration error before it becomes routable.
    const retiredEndpoint=new URL(`/v1/agents/${thread}/tool-host`,base);retiredEndpoint.protocol="ws:";
    const retired=createAttachment(tools,{endpoint:retiredEndpoint.href,transport:{connect(){return new WebSocket(retiredEndpoint,{headers});}}},
      {machines:[{id:localMachine,name:"Synthetic Session Hand",workspace,capabilities:["shell"]}],attachmentId:localMachine});
    await assert.rejects(retired.connect(),/hand_migration_required/);await retired.close().catch(()=>{});
    // The second computer attaches once at account scope, like every physical Hand.
    const localEndpoint=new URL("/tool-host",base);localEndpoint.protocol="ws:";
    const connectLocal=()=>createAttachment(tools,{endpoint:localEndpoint.href,transport:{connect(){
      const socket=new WebSocket(localEndpoint,{headers:{"x-nanocodex-owner-id":owner}}),send=socket.send.bind(socket);
      socket.send=(data,...args)=>{wire.push({direction:"local-host",frame:JSON.parse(String(data))});return send(data,...args);};
      socket.on("message",data=>wire.push({direction:"local-broker",frame:JSON.parse(String(data))}));return socket;
    }}},{machines:[{id:localMachine,name:"Synthetic Session Hand",workspace,capabilities:["shell"]}],attachmentId:localMachine});
    localAttachment=connectLocal();
    assert.equal((await localAttachment.connect()).connected,true);
    let number=100;
    const runTurn=async scenario=>{const started=performance.now(),id=`00000000-0000-7000-8000-${String(number++).padStart(12,"0")}`;
      const accepted=await request(`/v1/agents/${thread}/turns`,{method:"POST",body:JSON.stringify({id,input:`PREPARATION_${scenario}`})});assert.equal(accepted.status,202,JSON.stringify(accepted));
      for(let i=0;i<1500;i++){const response=await request(`/v1/agents/${thread}/turns/${accepted.value.turn_id}`);assert.equal(response.status,200);assert.ok(!["failed","cancelled"].includes(response.value.state),JSON.stringify(response));if(response.value.state==="completed")return {turn:response.value,elapsed_ms:performance.now()-started};await delay(10);}throw Error("turn deadline");};
    // Warm startup's optional inventory first; measured cells still demand fresh snapshots.
    for(const scenario of trials.slice(0,2)) {const value=await runTurn(scenario);assert.match(JSON.stringify(value.turn),/PREP_OK/);}
    assert.equal((await request("/account-tools/__fixture",{method:"POST",body:JSON.stringify({delay_ms:120})})).status,204);
    for(const scenario of trials.slice(2)) {
      const first=records.length,value=await runTurn(scenario),current=records.slice(first),stages=current.filter(row=>row.type==="hand.tool.stage"&&row.parent_call_id===`call_preparation_${scenario}`&&row.stage==="namespace.prepare");
      assert.match(JSON.stringify(value.turn),/PREP_OK/);assert.doesNotMatch(JSON.stringify(value.turn),/UNSAFE_VM/);assert.equal(stages.length,1,"concurrent calls must join one cell preparation");
      assert.equal(current.filter(row=>row.type==="fixture.pool"&&row.stage==="start").length,prepSource?2:0);
      assert.equal(current.filter(row=>row.type==="fixture.snapshot"&&row.stage==="start").length,1);
      const snapshot=current.find(row=>row.type==="fixture.snapshot"&&row.stage==="start");
      const selectedLookup=current.find(row=>row.type==="hand.tool.stage"&&row.stage==="namespace.selected_lookup");
      if(!prepSource) {assert.equal(snapshot.selected,machine);assert.ok(selectedLookup);}
      const readiness=current.find(row=>row.type==="hand.tool.stage"&&row.parent_call_id===`call_preparation_${scenario}`&&row.stage==="namespace.host_readiness");
      const discovery=current.find(row=>row.type==="hand.tool.stage"&&row.parent_call_id===`call_preparation_${scenario}`&&row.stage==="namespace.account_discovery");
      samples.push({scenario,prepare_ms:stages[0].duration_ms,public_turn_ms:value.elapsed_ms,selected_machine:snapshot.selected, selected_lookup_ms:selectedLookup?.duration_ms, snapshots:current.filter(row=>row.type==="fixture.snapshot"&&row.stage==="start").length, pools:current.filter(row=>row.type==="fixture.pool"&&row.stage==="start").length,
        ...(readiness?{host_readiness_ms:readiness.duration_ms}:{}),...(discovery?{account_discovery_ms:discovery.duration_ms}:{})});
    }
    const localSamples=[];
    for(let i=0;i<7;i++) {
      const scenario=`DIRECT_MEASURE_${i}`,start=records.length,value=await runTurn(scenario),rows=records.slice(start);
      assert.match(JSON.stringify(value.turn),/LOCAL_INDEPENDENT/);
      const stage=rows.find(row=>row.type==="hand.tool.stage"&&row.parent_call_id===`call_preparation_${scenario}`&&row.stage==="namespace.prepare");
      assert.ok(stage);
      const snapshots=rows.filter(row=>row.type==="fixture.snapshot"&&row.stage==="start").length;
      const pools=rows.filter(row=>row.type==="fixture.pool"&&row.stage==="start").length;
      // The second computer is an account Hand. Once discovered, each fresh
      // cell performs one selected-Hand lookup and never prepares VM pools.
      assert.equal(snapshots,1);
      if(i>0) {assert.equal(pools,0);
        assert.ok(rows.filter(row=>row.type==="fixture.snapshot"&&row.stage==="start").every(row=>row.selected===localMachine),"only the selected account Hand is looked up");}
      if(i>=2) localSamples.push({scenario,prepare_ms:stage.duration_ms,public_turn_ms:value.elapsed_ms,snapshots,pools});
    }
    const processStart=records.length;
    const processTurn=await runTurn("PROCESS");assert.match(JSON.stringify(processTurn.turn),/PROCESS_RESUMED/);
    if(!prepSource) {
      // Only the starting exec looks up its selected account Hand; retained
      // process polling resolves its original binding without inventory.
      const processRows=records.slice(processStart);
      assert.equal(processRows.filter(row=>row.type==="fixture.pool").length,0);
      assert.deepEqual(processRows.filter(row=>row.type==="fixture.snapshot"&&row.stage==="start").map(row=>row.selected),[localMachine],"retained process polling must resolve its original binding without inventory");
    }
    const inventory=await runTurn("INVENTORY");
    for(const id of [machine,localMachine]) assert.match(JSON.stringify(inventory.turn),new RegExp(id));
    const mixed=await runTurn("MIXED");
    for(const marker of ["LOCAL_FIRST","ACCOUNT_SECOND","LOCAL_AGAIN","unknown"]) assert.match(JSON.stringify(mixed.turn),new RegExp(marker));
    if(!prepSource) {
    // Hold only the other Hand's selected lookup, after this cell pinned the
    // local account Hand's first generation, then reconnect that Hand.
    assert.equal((await request("/account-tools/__fixture",{method:"POST",body:JSON.stringify({hold_selected:machine})})).status,204);
    const reconnectStart=records.length;
    const reconnectTurn=runTurn("RECONNECT");
    void reconnectTurn.catch(()=>{});
    const heldLookup=()=>records.slice(reconnectStart).some(row=>row.type==="fixture.snapshot"&&row.stage==="start"&&row.selected===machine);
    for(let i=0;i<1000&&!heldLookup();i++) await delay(5);
    assert.ok(heldLookup());
    await localAttachment.close();localAttachment=connectLocal();assert.equal((await localAttachment.connect()).connected,true);
    assert.equal((await request("/account-tools/__fixture",{method:"POST",body:JSON.stringify({delay_ms:0})})).status,204);
    const reconnected=await reconnectTurn;
    assert.match(JSON.stringify(reconnected.turn),/stale_route/);
    assert.doesNotMatch(JSON.stringify(reconnected.turn),/WRONG_GENERATION/);
    const recovered=await runTurn("RECOVER");assert.match(JSON.stringify(recovered.turn),/LOCAL_INDEPENDENT/);
    }
    const excluded=await runTurn("EXCLUDED");
    assert.doesNotMatch(JSON.stringify(excluded.turn),/UNSAFE_DISPATCH/);
    for(const root of ["/slow-vm","/fast-vm","/unknown-hand"]) assert.ok(JSON.stringify(excluded.turn).includes(root));
    const before=wire.filter(row=>row.direction==="broker"&&row.frame.type==="call").length;
    const omitted=await runTurn("OMITTED");assert.match(JSON.stringify(omitted.turn),/OMITTED_BRAIN/);
    assert.ok(!records.some(row=>row.type==="hand.tool.stage"&&row.parent_call_id==="call_preparation_OMITTED"&&row.stage==="namespace.prepare"));
    const brain=await runTurn("BRAIN");assert.match(JSON.stringify(brain.turn),/BRAIN_INDEPENDENT/);
    assert.ok(!records.some(row=>row.type==="hand.tool.stage"&&row.parent_call_id==="call_preparation_BRAIN"&&row.stage==="namespace.prepare"));
    assert.equal((await request("/account-tools/__fixture",{method:"POST",body:JSON.stringify({delay_ms:30,fail:true})})).status,204);
    const failedId=`00000000-0000-7000-8000-${String(number++).padStart(12,"0")}`;
    const failureAdmission=await request(`/v1/agents/${thread}/turns`,{method:"POST",body:JSON.stringify({id:failedId,input:"PREPARATION_FAIL"})});
    assert.equal(failureAdmission.status,202);
    for(let i=0;i<1000&&!records.some(row=>row.type==="hand.tool.stage"&&row.stage==="namespace.prepare"&&row.parent_call_id==="call_preparation_FAIL"&&row.outcome==="failed");i++) await delay(5);
    assert.ok(records.some(row=>row.type==="hand.tool.stage"&&row.stage==="namespace.prepare"&&row.parent_call_id==="call_preparation_FAIL"&&row.outcome==="failed"));
    // Discovery failures enter managed recovery. Exercise the user's public
    // cancel surface rather than having the fixture model fabricate a retry.
    const cancel=await request(`/v1/agents/${thread}/turns/${failedId}/cancel`,{method:"POST"});
    assert.ok([200,202].includes(cancel.status),JSON.stringify(cancel));
    let failedTurn;
    for(let i=0;i<1000;i++) {failedTurn=await request(`/v1/agents/${thread}/turns/${failedId}`);if(["cancelled","failed","completed"].includes(failedTurn.value.state))break;await delay(5);}
    assert.equal(failedTurn.value.state,"cancelled",JSON.stringify(failedTurn));
    assert.equal(wire.filter(row=>row.direction==="broker"&&row.frame.type==="call").length,before,"discovery failure and Brain must never dispatch to a Hand");
    const effects=(await readFile(join(workspace,"effect.log"),"utf8")).trim().split("\n").sort();
    assert.deepEqual(effects,trials.flatMap(scenario=>[`${scenario}:a`,`${scenario}:b`]).sort());assert.equal(before,prepSource?15:16);
    if(!prepSource) {
      await attachment.close();
      assert.equal((await request("/account-tools/__fixture",{method:"POST",body:JSON.stringify({delay_ms:0})})).status,204);
      const offlineStart=records.length;
      const offline=await runTurn("OFFLINE");
      assert.match(JSON.stringify(offline.turn),/offline/);
      assert.doesNotMatch(JSON.stringify(offline.turn),/MUST_NOT_RUN_OFFLINE/);
      const offlineRows=records.slice(offlineStart);
      assert.equal(offlineRows.filter(row=>row.type==="fixture.snapshot"&&row.stage==="start").length,1);
      assert.equal(offlineRows.find(row=>row.type==="fixture.snapshot"&&row.stage==="start").selected,machine);
      assert.equal(offlineRows.filter(row=>row.type==="fixture.pool").length,0);
      assert.equal(wire.filter(row=>row.direction==="broker"&&row.frame.type==="call").length,before);
    }
    const diagnostics=await request(`/v1/agents/${thread}/diagnostics?limit=1024`);assert.equal(diagnostics.status,200);
    if(!prepSource) {
      const managed=diagnostics.value.services.find(service=>service.service==="managed");
      assert.equal(managed.available,true);
      // The additional route journeys can fill a diagnostics page. Follow the
      // public cursor before asserting on the final failure/cancelled turn.
      let after=managed.next_after;
      for (;;) {
        const next=await request(`/v1/agents/${thread}/diagnostics?limit=1024&after_managed=${after}`);
        assert.equal(next.status,200);
        const page=next.value.services.find(service=>service.service==="managed");
        assert.equal(page.available,true);
        managed.events.push(...page.events);
        if (page.next_after===after) break;
        after=page.next_after;
      }
      assert.ok(managed.events.some(event=>event.stage==="namespace.prepare"&&event.parent_call_id==="call_preparation_MEASURE_2"&&event.outcome==="ok"));
      const failedRows=managed.events.filter(event=>event.parent_call_id==="call_preparation_FAIL");
      assert.ok(failedRows.some(event=>event.stage==="namespace.prepare"&&event.outcome==="failed"));

    }
    await writeFile(join(output,"diagnostics.json"),JSON.stringify(diagnostics.value,null,2));
    result.observed={once_only_effects:effects.length,concurrent_cell_capture_count:7,unavailable_vms_excluded:true,brain_bypasses_prepare:true,discovery_failure_predispatch:true,failed_discovery_cancelled:true,thread_native_catalog_rejected:true,mixed_cell_routes:true,known_offline_selected_only:!prepSource,reconnect_keeps_pinned_generation:!prepSource,fresh_cell_recovers:!prepSource,process_resumed:true,full_inventory:true,samples,localSamples};
    console.log(JSON.stringify({evidence:output,label,...result.observed}));
  } catch(error){failure=error;result.error=error.stack;throw error;}
  finally {try{await localAttachment?.close();await attachment?.close();await tools?.close();await native?.close();await mf?.dispose();}finally{
    await writeFile(join(output,"trace.json"),JSON.stringify({result,records},null,2));await writeFile(join(output,"wire.json"),JSON.stringify(wire,null,2));await writeFile(join(output,"http.json"),JSON.stringify(http,null,2));await writeFile(join(output,"runtime.log"),runtime.join("\n")+"\n");
    await writeFile(join(output,"README.md"),`Run: \`${command}\`\n\nLabel: ${label}\nStatus: ${failure?"FAIL: "+failure.message:"PASS"}\nInputs: ${JSON.stringify(result.inputs)}\nExpected: ${JSON.stringify(result.expected)}\nObserved: ${JSON.stringify(result.observed)}\n\nActual public HTTP turn admission/results and shipped fresh-cell Session callback, WASM Code Mode, account discovery, SQLite ledger, reverse WebSocket and native /bin/sh. Synthetic external VM pool negative receipts wait 160/40ms; a wrapper around the real account DO adds 120ms only to full inventory snapshots; selected-machine snapshots use the actual account DO immediately. These injected waits demonstrate overlap only, not production network latency. First two cells excluded as warmups. Optional NANOCODEX_NAMESPACE_PREP_SOURCE bundles a saved baseline index.ts with all other source resolutions unchanged (source-resolution.json hashes). Retain paired runs and outliers. trace.json, wire.json, http.json, diagnostics.json, runtime.log, fixture-source.mjs, worker.mjs and SQLite are the inspection evidence.\n`);
  }}
});
