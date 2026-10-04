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
import { crc32 } from "node:zlib";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000071";
const thread = "00000000-0000-7000-8000-000000000072";
const organization = "00000000-0000-7000-8000-000000000073";
const team = "00000000-0000-7000-8000-000000000074";
const command = "node --test js/managed/test/code-mode-async-journey.test.mjs";
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
export { AccountHostedTools };
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const path=new URL(request.url).pathname;
    if(path==='/__inspect') return Response.json({jobs:this.ctx.storage.sql.exec('SELECT id,state,delivery_turn_id,runtime_turn_id,turn_id,operation_id,model_call_index FROM managed_async_code_jobs').toArray(),effects:this.ctx.storage.sql.exec('SELECT name,state FROM managed_code_effects').toArray(),turns:this.ctx.storage.sql.exec('SELECT id,state FROM managed_turns').toArray()});
    if(path==='/__pin-missing') { this.ctx.storage.sql.exec("INSERT INTO managed_turns(id,request_key,request_hash,input_json,authorization_json,state,accepted_cursor,may_have_inner_operation,created_at,accepted_at,updated_at) SELECT 'missing-target','missing-target',request_hash,input_json,authorization_json,'completed',accepted_cursor,1,created_at,accepted_at,updated_at FROM managed_turns LIMIT 1"); this.ctx.storage.sql.exec("UPDATE managed_async_code_jobs SET delivery_turn_id='missing-target'");await this.ctx.storage.sync();return new Response(null,{status:204}); }
    if(path==='/__pin-terminal') { this.ctx.storage.sql.exec('UPDATE managed_async_code_jobs SET delivery_turn_id=turn_id');await this.ctx.storage.sync();return new Response(null,{status:204}); }
    if(path==='/__revoke') { this.ctx.storage.sql.exec('UPDATE session_state SET authorization_epoch=2');await this.ctx.storage.sync();return new Response(null,{status:204}); }
    if(path==='/__alarm') { await this.alarm(); return new Response(null,{status:204}); }
    if(path==='/__seed') {
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://fixture.internal/','managed',?)",'${thread}','${owner}','${organization}','${team}',Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES(1,?)",JSON.stringify({code_mode_async:scenario.name!=="default-off",multi_agent:{enabled:true},environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  index=0; collected=[]; childCalls=0;
  async fetch(request) {
    if(request.headers.get('upgrade')!=='websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client,server]=Object.values(new WebSocketPair()); server.accept(); let childSocket=false;
    server.addEventListener('close',()=>server.close(1000));
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data),call=++this.index;
      childSocket ||= JSON.stringify((body.input??[]).filter(item=>item.role==='user')).includes('ASYNC_CHILD_FIXTURE');
      const childCall=childSocket?++this.childCalls:0;
      console.info({type:'fixture.model',index:call,child:childSocket,input:body.input,tools:body.tools});
      const outputs=(body.input??[]).filter(item=>item.type==='custom_tool_call_output'||item.type==='function_call_output'); this.collected.push(...outputs);
      const terminal=JSON.stringify(this.collected).includes(['chain','same-turn'].includes(scenario.name)?'CHAIN_EFFECT_DONE':'ASYNC_EFFECT_DONE');
      const interrupted=JSON.stringify(outputs).includes('interrupted before a terminal receipt');
      const output=interrupted?[{type:'message',role:'assistant',content:[{type:'output_text',text:'ASYNC_INTERRUPTED_OK'}]}]:childSocket?(childCall===1?[{type:'custom_tool_call',name:'exec',call_id:'call_child_sync',input:'text("CHILD_NORMAL_OK");'}]:childCall===2?[{type:'function_call',name:'submit_result',call_id:'child_submit',arguments:JSON.stringify({output:'CHILD_NORMAL_OK'})}]:[{type:'message',role:'assistant',content:[{type:'output_text',text:'CHILD_NORMAL_OK'}]}]):call===1?[{type:'custom_tool_call',name:'exec',call_id:'call_async_once',input:scenario.script}]:(scenario.name==='chain'&&call===3||scenario.name==='same-turn'&&call===2)?[{type:'custom_tool_call',name:'exec',call_id:'call_async_once',input:'await new Promise(resolve => setTimeout(resolve, 1200)); text(await tools.exec_command({cmd:"printf CHAIN_EFFECT_DONE"}));'}]
        :[{type:'message',role:'assistant',content:[{type:'output_text',text:terminal?'ASYNC_DELIVERED_OK':'JOB_STARTED_OK'}]}];
      const send=()=>server.send(JSON.stringify({type:'response.completed',response:{id:'resp_async_'+call,status:'completed',end_turn:interrupted||childSocket?interrupted||childCall>2:call>1&&!(scenario.name==='chain'&&call===3||scenario.name==='same-turn'&&call===2),output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
      if((scenario.name==='active'||scenario.name==='active-stop')&&call===3) setTimeout(send,1800);else send();
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

const hash=bytes=>createHash("sha256").update(bytes).digest("hex");

// A valid PNG with a large ancillary text chunk exercises chunked SQLite receipts.
const tinyPng=Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l1cAAAAASUVORK5CYII=","base64");
const textChunk=Buffer.concat([Buffer.from("tEXt"),Buffer.from("fixture\0"+"a".repeat(300000))]);
const chunkLength=Buffer.alloc(4);chunkLength.writeUInt32BE(textChunk.length-4);
const chunkCrc=Buffer.alloc(4);chunkCrc.writeUInt32BE(crc32(textChunk));
const largePng="data:image/png;base64,"+Buffer.concat([tinyPng.subarray(0,-12),chunkLength,textChunk,chunkCrc,tinyPng.subarray(-12)]).toString("base64");
const scenarios=[
  {name:"cold",script:'await new Promise(resolve => setTimeout(resolve, 5000)); text(await tools.exec_command({cmd:"printf MUST_NOT_REPLAY"}));'},
  {name:"default-off",script:'text("ASYNC_EFFECT_DONE");'},
  {name:"missing-target",script:'await new Promise(resolve => setTimeout(resolve, 1500)); text("ASYNC_EFFECT_DONE");'},
  {name:"unsupported",script:'text("ASYNC_EFFECT_DONE");'},
  {name:"idle",script:'await new Promise(resolve => setTimeout(resolve, 350)); text("ASYNC_EFFECT_DONE");'},
  {name:"same-turn",script:'await new Promise(resolve => setTimeout(resolve, 900)); text("ASYNC_EFFECT_DONE");'},
  {name:"chain",script:'await new Promise(resolve => setTimeout(resolve, 350)); text("ASYNC_EFFECT_DONE");'},
  {name:"active",script:'await new Promise(resolve => setTimeout(resolve, 900)); text("ASYNC_EFFECT_DONE");'},
  {name:"active-stop",script:'await new Promise(resolve => setTimeout(resolve, 1500)); text(await tools.exec_command({cmd:"printf UNAUTHORIZED_EFFECT"})); text("ASYNC_EFFECT_DONE");'},
  {name:"terminal-target",script:'await new Promise(resolve => setTimeout(resolve, 1500)); text("ASYNC_EFFECT_DONE");'},
  {name:"image",script:'await new Promise(resolve => setTimeout(resolve, 350)); image('+JSON.stringify(largePng)+'); text("ASYNC_EFFECT_DONE");'},
  {name:"stop",script:'await new Promise(resolve => setTimeout(resolve, 1500)); text(await tools.exec_command({cmd:"printf UNAUTHORIZED_EFFECT"})); text("ASYNC_EFFECT_DONE");'},
  {name:"revoke",script:'await new Promise(resolve => setTimeout(resolve, 1500)); text(await tools.exec_command({cmd:"printf UNAUTHORIZED_EFFECT"})); text("ASYNC_EFFECT_DONE");'},
  {name:"child",script:'const child = await tools.spawn_agent({role:"Fixture child",task:"ASYNC_CHILD_FIXTURE: emit CHILD_NORMAL_OK and submit it.",model:"gpt-6.1-sol",thinking:"low",output_contract:{kind:"string"}}); text(await tools.wait_agent({agent_ids:[child.agent_id],timeout_ms:10000})); text("ASYNC_EFFECT_DONE");'},
];
for(const scenario of scenarios) test("managed async journey: "+scenario.name, {timeout:120000}, async () => {
  const output=join(repo,"output/async-managed-journey",`${Date.now()}-${process.pid}-${scenario.name}`);
  await mkdir(output,{recursive:true});
  const selectedRouter=join(repo,"js/nanocodex-tools/runtime/tool-router.mjs");
  const records=[],runtime=[],http=[];
  const capture=line=>{runtime.push(line);const start=line.indexOf('{"type":');if(start>=0)try{records.push(JSON.parse(line.slice(start)));}catch{}};
  let mf;
  try {
    const assets=[],routerLoads=[];
    const routerBytes=await readFile(selectedRouter);
    const plugins=[{name:"exact-router-source",setup(builder){
      builder.onLoad({filter:/\/nanocodex-tools\/runtime\/tool-router\.mjs$/},args=>{
        routerLoads.push(args.path);return {contents:routerBytes.toString(),loader:"js",resolveDir:join(repo,"js/nanocodex-tools/runtime")};
      });
    }},{name:"wasm",setup(builder){builder.onResolve({filter:/\.wasm$/},async args=>{
      const path=join(args.resolveDir,args.path),contents=await readFile(path),name=`fixture-${assets.length}.wasm`;
      assets.push({type:"CompiledWasm",path:name,contents});return {path:`./${name}`,external:true};
    });}}];
    const bundle=await build({stdin:{contents:"const scenario="+JSON.stringify(scenario)+";\n"+source,resolveDir:root},bundle:true,write:false,metafile:true,format:"esm",platform:"node",conditions:["workerd"],target:"es2022",
      banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:["cloudflare:*","node:*"],
      alias:{"nanocodex-tools/runtime/tool-router":join(repo,"js/nanocodex-tools/runtime/tool-router.mjs"),"nanocodex-tools/hosted":join(repo,"js/nanocodex-tools/src/hosted/index.ts"),"node-rsa":join(root,"../nanocodex/tools/browser/unsupportedNodeRsa.mjs")},plugins,logLevel:"silent"});
    assert.equal(routerLoads.length,1,"exact production router source must be selected once, not stale dist");
    const hashes=Object.fromEntries(await Promise.all(["js/managed/src/async-code-jobs.ts","js/nanocodex-tools/runtime/code-runtime.mjs","js/nanocodex/runtime/code-effect-identity.mjs","js/managed/src/index.ts","js/managed/src/account-hosted-tools.ts","js/managed/src/hand-call-observation.ts","js/nanocodex-tools/src/hosted/broker-core.ts","js/nanocodex-tools/tools/nodeProcess.mjs","js/nanocodex/tools/Tools.mjs"].map(async path=>[path,hash(await readFile(join(repo,path)))])));
    await writeFile(join(output,"source-resolution.json"),JSON.stringify({selectedRouter,router_sha256:hash(routerBytes),host_router_sha256:hash(await readFile(join(repo,"js/nanocodex-tools/runtime/tool-router.mjs"))),worker_sha256:hash(bundle.outputFiles[0].text),hashes,routerLoads,bundleInputs:Object.keys(bundle.metafile.inputs),wasm:assets.map(asset=>({path:asset.path,sha256:hash(asset.contents),bytes:asset.contents.length}))},null,2));
    await writeFile(join(output,"worker.mjs"),bundle.outputFiles[0].text);
    const date="2026-07-30";
    const miniflareOptions={port:0,unsafeLocalExplorer:true,unsafeObservability:true,handleRuntimeStdio(stdout,stderr){createInterface({input:stdout}).on("line",capture);createInterface({input:stderr}).on("line",capture);},durableObjectsPersist:join(output,"sqlite"),workers:[
      {name:"managed",compatibilityDate:date,compatibilityFlags:["nodejs_compat","enable_request_signal"],modules:[{type:"ESModule",path:"worker.mjs",contents:bundle.outputFiles[0].text},...assets],bindings:{AGENT_IDLE_TIMEOUT_MS:"60000"},
        durableObjects:{NANOCODEX_SESSIONS:{className:"FixtureSession",useSQLite:true},NANOCODEX_ACCOUNT_TOOLS:{className:"AccountHostedTools",useSQLite:true},NANOCODEX_MEMORY:{className:"FixtureModel",useSQLite:true},MODEL:{className:"FixtureModel",useSQLite:true}},serviceBindings:{NANOCODEX:"provider"},r2Buckets:["NANOCODEX_HISTORY","NANOCODEX_WORKSPACES"]},
      {name:"provider",compatibilityDate:date,modules:true,script:"export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}}",durableObjects:{MODEL:{className:"FixtureModel",scriptName:"managed",useSQLite:true}}}]};
    mf=new Miniflare(miniflareOptions);

    let base=await mf.ready;
    const headers={"x-nanocodex-owner-id":owner,"x-nanocodex-session-organization-id":organization,"x-nanocodex-session-team-id":team,"x-nanocodex-authorization-epoch":"1","x-nanocodex-capabilities":JSON.stringify(["agents:read","agents:write","tools:use"]),"content-type":"application/json"};
    const request=async(path,init={})=>{const response=await fetch(new URL(path,base),{...init,headers:{...headers,...init.headers},signal:AbortSignal.timeout(10000)}),body=await response.text();http.push({path,status:response.status,body});return {status:response.status,value:body?JSON.parse(body):undefined};};
    assert.equal((await request("/__seed",{method:"POST"})).status,204);
    if(scenario.name==="unsupported") {
      const settings=await request("/settings",{method:"PATCH",body:JSON.stringify({model:"claude-opus-5-5"})});
      assert.equal(settings.status,409,JSON.stringify(settings));
      assert.equal(settings.value.error,"async_harness_unsupported");
      const creation=await request("/initialize",{method:"PUT",body:JSON.stringify({session_id:thread,owner_id:owner,organization_id:organization,team_id:team,authorization_epoch:1,public_origin:"https://fixture.internal/",configuration:{code_mode_async:true},settings:{model:"claude-opus-5-5",thinking:"low",reasoning_mode:"standard",fast_mode:false}})});
      assert.equal(creation.status,409,JSON.stringify(creation));
      assert.equal(creation.value.error,"async_harness_unsupported");
      const session=await request("/state");
      assert.equal(session.value.settings.model,"gpt-6.1-sol");
      console.log(JSON.stringify({evidence:output,command,scenario:scenario.name,observed:{settings,creation}}));
      return;
    }
    const accepted=await request(`/v1/agents/${thread}/turns`,{method:"POST",body:JSON.stringify({id:"00000000-0000-7000-8000-000000000075",input:"Run the asynchronous script once; receive its result automatically."})});
    assert.equal(accepted.status,202,JSON.stringify(accepted));
    let events,activeSubmitted=false;
    for(let i=0;i<500;i++) {
      events=await request(`/v1/agents/${thread}/events/history?limit=256`);
      if(!activeSubmitted&&["active","active-stop"].includes(scenario.name)&&JSON.stringify(events).includes("JOB_STARTED_OK")) {
        activeSubmitted=true;
        assert.equal((await request(`/v1/agents/${thread}/turns`,{method:"POST",body:JSON.stringify({id:"00000000-0000-7000-8000-000000000076",input:"Keep this request active until the retained job completes."})})).status,202);
      }
      if(scenario.name==="active-stop"&&activeSubmitted&&records.filter(row=>row.type==="fixture.model").length>=3) break;
      if(JSON.stringify(events).includes(["cold","stop","revoke","terminal-target","missing-target"].includes(scenario.name)?"JOB_STARTED_OK":"ASYNC_DELIVERED_OK")) break;
      await delay(20);
    }
    assert.equal(events.status,200);
    if(scenario.name==="cold") {
      assert.match(JSON.stringify(events),/JOB_STARTED_OK/);
      await mf.dispose();
      mf=new Miniflare(miniflareOptions);
      base=await mf.ready;
      await request("/__alarm",{method:"POST"});
      for(let i=0;i<300;i++) {
        events=await request(`/v1/agents/${thread}/events/history?limit=256`);
        if(JSON.stringify(events).includes("ASYNC_INTERRUPTED_OK")) break;
        await delay(20);
      }
      assert.match(JSON.stringify(events),/ASYNC_INTERRUPTED_OK/);
      const retained=await request("/__inspect");
      assert.equal(retained.value.jobs.length,1);
      assert.equal(retained.value.jobs[0].state,"delivered",JSON.stringify(retained));
      assert.equal(retained.value.effects.length,0,"eviction must never replay unfinished guest code");
      console.log(JSON.stringify({evidence:output,command,scenario:scenario.name,observed:retained.value}));
      return;
    }
    if(["stop","revoke","active-stop","terminal-target","missing-target"].includes(scenario.name)) {
      assert.match(JSON.stringify(events),/JOB_STARTED_OK/);
      if(scenario.name==="active-stop") assert.equal((await request(`/v1/agents/${thread}/turns/00000000-0000-7000-8000-000000000076/cancel`,{method:"POST"})).status,202);
      else if(scenario.name==="missing-target") assert.equal((await request("/__pin-missing",{method:"POST"})).status,204);
      else if(scenario.name==="terminal-target") assert.equal((await request("/__pin-terminal",{method:"POST"})).status,204);
      else if(scenario.name==="stop") assert.equal((await request(`/v1/agents/${thread}/turns/00000000-0000-7000-8000-000000000075/cancel`,{method:"POST"})).status,200);
      else assert.equal((await request("/__revoke",{method:"POST"})).status,204);
      await delay(1800);
      await request("/__alarm",{method:"POST"});
      if(scenario.name==="terminal-target") {
        for(let i=0;i<200;i++) {
          events=await request(`/v1/agents/${thread}/events/history?limit=256`);
          if(JSON.stringify(events).includes("ASYNC_DELIVERED_OK")) break;
          await delay(20);
        }
        assert.match(JSON.stringify(events),/ASYNC_DELIVERED_OK/);
      }
      const retained=await request("/__inspect");
      assert.equal(retained.value.jobs.length,1);
      assert.equal(retained.value.jobs[0].state,scenario.name==="terminal-target"?"delivered":"cancelled",JSON.stringify(retained));
      assert.equal(retained.value.effects.length,0,JSON.stringify(retained));
      assert.equal(records.filter(row=>row.type==="fixture.model").length,["active-stop","terminal-target"].includes(scenario.name)?3:2);
      if(scenario.name==="missing-target") {
        const missing=await request(`/v1/agents/${thread}/events/history?limit=256`);
        assert.match(JSON.stringify(missing),/async_delivery_unknown/);
        await request("/__alarm",{method:"POST"});await delay(100);
        const replay=await request(`/v1/agents/${thread}/events/history?limit=256`);
        assert.equal((JSON.stringify(replay).match(/async_delivery_unknown/g)??[]).length,1);
        assert.equal(records.filter(row=>row.type==="fixture.model").length,2);
      }
      console.log(JSON.stringify({evidence:output,command,scenario:scenario.name,observed:retained.value}));
      return;
    }
    assert.match(JSON.stringify(events),/ASYNC_DELIVERED_OK/);
    if(scenario.name==="default-off") {
      const before=await request("/state");
      assert.equal(before.value.agent_loaded,true,JSON.stringify(before));
      assert.equal((await request(`/v1/agents/${thread}/turns/00000000-0000-7000-8000-000000000075/cancel`,{method:"POST"})).status,200);
      const after=await request("/state"),retained=await request("/__inspect");
      assert.equal(after.value.agent_loaded,true,JSON.stringify(after));
      assert.equal(retained.value.jobs.length,0);
      assert.equal(records.filter(row=>row.type==="fixture.model").length,2);
      console.log(JSON.stringify({evidence:output,command,scenario:scenario.name,observed:{before:before.value.agent_loaded,after:after.value.agent_loaded,jobs:0}}));
      return;
    }
    const calls=records.filter(row=>row.type==="fixture.model");
    assert.ok(calls.length>=3,"expected separate admission and terminal inference requests");
    assert.match(JSON.stringify(calls),/Script admitted with job ID/);
    const continuations=calls.filter(row=>(row.input??[]).some(item=>item.call_id?.startsWith("async_")&&item.type==="custom_tool_call_output"));
    assert.equal(continuations.length,["chain","same-turn"].includes(scenario.name)?2:1,"completion must be delivered once as tool output");
    if(["chain","same-turn"].includes(scenario.name)) {
      const retained=await request("/__inspect");
      assert.equal(retained.value.jobs.length,2,JSON.stringify(retained));
      assert.ok(retained.value.jobs.every(job=>job.state==="delivered"));
      if(scenario.name==="same-turn") {
        assert.equal(new Set(retained.value.jobs.map(job=>job.operation_id)).size,1);
        assert.equal(new Set(retained.value.jobs.map(job=>job.model_call_index)).size,2,"repeated provider call ID must retain independent model-call jobs");
      }
      assert.equal(retained.value.effects.filter(effect=>effect.name==="exec_command").length,1,JSON.stringify(retained));
      assert.match(JSON.stringify(calls),/CHAIN_EFFECT_DONE/);
      assert.ok(!JSON.stringify(events).includes('"kind":"completion"'),"internal completion acceptance must not broadcast as user input");
    }
    if(scenario.name==="image") {
      const image=continuations.flatMap(row=>row.input??[]).filter(item=>item.type==="custom_tool_call_output").flatMap(item=>Array.isArray(item.output)?item.output:[]).find(item=>item.type==="input_image");
      assert.equal(image?.image_url,largePng,"typed image larger than 256KiB must reach inference intact");
    }
    if(scenario.name==="child") {
      const childCalls=calls.filter(row=>row.child);
      assert.ok(childCalls.length>=2,"child must run through the native spawning bridge");
      assert.match(JSON.stringify(childCalls),/CHILD_NORMAL_OK/);
      assert.ok(!JSON.stringify(childCalls).includes("Script admitted with job ID"),"child exec remains synchronous");
    }
    assert.ok(!calls.some(row=>(row.input??[]).some(item=>item.role==="user"&&JSON.stringify(item).includes("ASYNC_EFFECT_DONE"))));
    if(scenario.name==="active") {
      const retained=await request("/__inspect");
      assert.equal(retained.value.jobs[0].delivery_turn_id,"00000000-0000-7000-8000-000000000076");
      assert.equal(retained.value.jobs[0].state,"delivered");
    }
    const beforeReplay=records.filter(row=>row.type==="fixture.model").length;
    await request("/__alarm",{method:"POST"});await delay(100);
    assert.equal(records.filter(row=>row.type==="fixture.model").length,beforeReplay,"alarm replay must not redeliver terminal output");
    console.log(JSON.stringify({evidence:output,command,observed:{model_requests:calls.length,terminal_tool_deliveries:continuations.length}}));
  } finally {await mf?.dispose();await writeFile(join(output,"runtime.log"),runtime.join("\n"));await writeFile(join(output,"http.json"),JSON.stringify(http,null,2));await writeFile(join(output,"records.json"),JSON.stringify(records,null,2));}
});
