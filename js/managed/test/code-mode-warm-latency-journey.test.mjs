import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { createTools } from "../../nanocodex/tools/Tools.mjs";
import { createAttachment } from "../../nanocodex-tools/tools/attachment.mjs";
import { createNodeProcessTools } from "../../nanocodex-tools/tools/nodeProcess.mjs";
import { fetch } from "./support/miniflare-fetch.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000071";
const thread = "00000000-0000-7000-8000-000000000072";
const organization = "00000000-0000-7000-8000-000000000073";
const team = "00000000-0000-7000-8000-000000000074";
const machine = "synthetic-warm-hand";
const command = "node --test js/managed/test/code-mode-warm-latency-journey.test.mjs";
const count = 24;
const scripts = [0, 1].map(cell => `
  text({cell: ${cell}, catalog_count: ALL_TOOLS.length});
  for (let offset = 0; offset < 12; offset++) {
    const index = ${cell * 12} + offset;
    const began = Date.now();
    const result = await tools.exec_command({cmd: "printf '" + index + "\\n' >> effects.log; printf 'WARM_OK_" + index + "'", workdir: "/${machine}", shell: "/bin/sh", login: false, yield_time_ms: 1000});
    if (result.exit_code !== 0 || result.output !== "WARM_OK_" + index) throw new Error("unexpected native output " + JSON.stringify(result));
    text({index, output: result.output, exit_code: result.exit_code, elapsed_ms: Date.now() - began});
  }
`);

// Only synthetic seed/admission and the external model are substituted. Actual
// Session, WASM Code Mode, discovery, AccountHostedTools, SQLite, native shell,
// public HTTP and reverse WebSocket run unmodified, without injected timings.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
export { AccountHostedTools };
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    if(new URL(request.url).pathname==='/__seed') {
      // A fresh Session creates its tables on its first request; this one is refused without an owner.
      await (await super.fetch(new Request('https://session.internal/sites'))).body?.cancel();
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://fixture.internal/','managed',?)",'${thread}','${owner}','${organization}','${team}',Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES(1,?)",JSON.stringify({environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if(request.headers.get('upgrade')!=='websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client,server]=Object.values(new WebSocketPair()); server.accept(); let index=0,collected=[];
    server.addEventListener('close',()=>server.close(1000));
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data),call=++index;
      console.info({type:'fixture.model',index:call,input:body.input,tools:body.tools});
      const outputs=(body.input??[]).filter(item=>item.type==='custom_tool_call_output'||item.type==='function_call_output'); collected.push(...outputs);
      const output=call===1?[{type:'custom_tool_call',name:'exec',call_id:'call_warm_discovery',input:'text(await tools.tool_search({query:"exec_command",limit:8}));'}]
        :call===2||call===3?[{type:'custom_tool_call',name:'exec',call_id:'call_warm_cell_'+(call-2),input:${JSON.stringify(scripts)}[call-2]}]
        :[{type:'message',role:'assistant',content:[{type:'output_text',text:'WARM_THREAD_OK '+JSON.stringify(collected)}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_warm_'+call,status:'completed',end_turn:call>3,output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
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

function summarize(values) {
  const ordered = values.toSorted((a,b) => a-b);
  if (!ordered.length) return { n: 0 };
  const percentile = p => ordered[Math.min(ordered.length - 1, Math.ceil(p * ordered.length) - 1)];
  return { n: values.length, min: ordered[0], p50: percentile(.5), p95: percentile(.95), max: ordered.at(-1), mean: values.reduce((a,b)=>a+b,0)/values.length };
}
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

test("one running managed thread measures sequential warm Code Mode calls after discovery", { timeout: 120_000 }, async () => {
  const label = process.env.CODE_MODE_WARM_LABEL ?? `candidate-${Date.now()}-${process.pid}`;
  assert.match(label,/^[a-zA-Z0-9_-]+$/);
  const output = join(repo,"output/code-mode-warm-latency-20261002",label);
  const workspace = join(output,"hand");
  await mkdir(workspace,{recursive:true});
  const selectedRouter = process.env.CODE_MODE_WARM_SOURCE ? resolve(process.env.CODE_MODE_WARM_SOURCE) : join(repo,"js/nanocodex-tools/runtime/tool-router.mjs");
  const records=[],wire=[],http=[],runtime=[];
  const result = {command,label,selectedRouter,inputs:{owner,thread,machine,scripts,count},expected:{native_calls:24,warm_calls:23,code_mode_cells:2,runtime_sessions:1,once_only_effects:24},observed:{}};
  // Save authored progress before boot, retaining failures and all later traces.
  await writeFile(join(output,"fixture-source.mjs"),source);
  await writeFile(join(output,"test-source.mjs"),await readFile(fileURLToPath(import.meta.url)));
  await writeFile(join(output,"progress.json"),JSON.stringify({status:"building",...result},null,2));
  const capture = line => {runtime.push(line);const start=line.indexOf('{"type":');if(start>=0){try{records.push(JSON.parse(line.slice(start)));}catch{}}};
  let mf,native,tools,attachment,failure;
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
    const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,metafile:true,format:"esm",platform:"node",conditions:["workerd"],target:"es2022",
      banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:["cloudflare:*","node:*"],
      alias:{"nanocodex-tools/runtime/tool-router":join(repo,"js/nanocodex-tools/runtime/tool-router.mjs"),"nanocodex-tools/hosted":join(repo,"js/nanocodex-tools/src/hosted/index.ts"),"node-rsa":join(root,"../nanocodex/tools/browser/unsupportedNodeRsa.mjs")},plugins,logLevel:"silent"});
    assert.equal(routerLoads.length,1,"exact production router source must be selected once, not stale dist");
    const hashes=Object.fromEntries(await Promise.all(["js/managed/src/index.ts","js/managed/src/account-hosted-tools.ts","js/managed/src/hand-call-observation.ts","js/nanocodex-tools/src/hosted/broker-core.ts","js/nanocodex-tools/tools/nodeProcess.mjs","js/nanocodex/tools/Tools.mjs"].map(async path=>[path,hash(await readFile(join(repo,path)))])));
    await writeFile(join(output,"source-resolution.json"),JSON.stringify({selectedRouter,router_sha256:hash(routerBytes),host_router_sha256:hash(await readFile(join(repo,"js/nanocodex-tools/runtime/tool-router.mjs"))),worker_sha256:hash(bundle.outputFiles[0].text),hashes,routerLoads,bundleInputs:Object.keys(bundle.metafile.inputs),wasm:assets.map(asset=>({path:asset.path,sha256:hash(asset.contents),bytes:asset.contents.length}))},null,2));
    await writeFile(join(output,"worker.mjs"),bundle.outputFiles[0].text);
    const date="2026-07-30";
    mf=new Miniflare({port:0,unsafeLocalExplorer:true,unsafeObservability:true,handleRuntimeStdio(stdout,stderr){createInterface({input:stdout}).on("line",capture);createInterface({input:stderr}).on("line",capture);},durableObjectsPersist:join(output,"sqlite"),workers:[
      {name:"managed",compatibilityDate:date,compatibilityFlags:["nodejs_compat","enable_request_signal"],modules:[{type:"ESModule",path:"worker.mjs",contents:bundle.outputFiles[0].text},...assets],bindings:{AGENT_IDLE_TIMEOUT_MS:"60000"},
        durableObjects:{NANOCODEX_SESSIONS:{className:"FixtureSession",useSQLite:true},NANOCODEX_ACCOUNT_TOOLS:{className:"AccountHostedTools",useSQLite:true},NANOCODEX_MEMORY:{className:"FixtureModel",useSQLite:true},MODEL:{className:"FixtureModel",useSQLite:true}},serviceBindings:{NANOCODEX:"provider"},r2Buckets:["NANOCODEX_HISTORY","NANOCODEX_WORKSPACES"]},
      {name:"provider",compatibilityDate:date,modules:true,script:"export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}}",durableObjects:{MODEL:{className:"FixtureModel",scriptName:"managed",useSQLite:true}}}]});
    const base=await mf.ready;
    const headers={"x-nanocodex-owner-id":owner,"x-nanocodex-session-organization-id":organization,"x-nanocodex-session-team-id":team,"x-nanocodex-authorization-epoch":"1","x-nanocodex-capabilities":JSON.stringify(["agents:read","agents:write","tools:use"]),"content-type":"application/json"};
    const request=async(path,init={})=>{const response=await fetch(new URL(path,base),{...init,headers:{...headers,...init.headers},signal:AbortSignal.timeout(10_000)}),body=await response.text();http.push({path,status:response.status,method:init.method??"GET",body});return {status:response.status,value:body?JSON.parse(body):undefined};};
    assert.equal((await request("/__seed",{method:"POST"})).status,204);
    native=await createNodeProcessTools({workspace});tools=await createTools({tools:native.tools});
    const endpoint=new URL("/tool-host",base);endpoint.protocol="ws:";
    attachment=createAttachment(tools,{endpoint:endpoint.href,transport:{connect(){const socket=new WebSocket(endpoint,{headers:{"x-nanocodex-owner-id":owner}}),send=socket.send.bind(socket);socket.send=(data,...args)=>{wire.push({at_ms:performance.now(),direction:"host",frame:JSON.parse(String(data))});return send(data,...args);};socket.on("message",data=>wire.push({at_ms:performance.now(),direction:"broker",frame:JSON.parse(String(data))}));return socket;}}},{machines:[{id:machine,name:"Synthetic Warm Hand",workspace,capabilities:["shell"]}],attachmentId:machine});
    assert.equal((await attachment.connect()).connected,true);
    await writeFile(join(output,"progress.json"),JSON.stringify({status:"connected",...result},null,2));
    const started=performance.now();
    const accepted=await request(`/v1/agents/${thread}/turns`,{method:"POST",body:JSON.stringify({id:"00000000-0000-7000-8000-000000000075",input:"Discover exec_command and run both sequential warm-call cells in this running thread."})});
    assert.equal(accepted.status,202,JSON.stringify(accepted));
    let completed;
    const deadline=Date.now()+80_000;
    do {const response=await request(`/v1/agents/${thread}/turns/${accepted.value.turn_id}`);assert.equal(response.status,200);completed=response.value;assert.ok(!["failed","cancelled"].includes(completed.state),JSON.stringify(completed));if(completed.state==="completed")break;await delay(10);}while(Date.now()<deadline);
    const publicTurnMs=performance.now()-started;
    assert.equal(completed.state,"completed",JSON.stringify(completed));
    assert.match(JSON.stringify(completed),/WARM_THREAD_OK/);
    for(let index=0;index<count;index++) assert.match(JSON.stringify(completed),new RegExp(`WARM_OK_${index}(?![0-9])`));
    const effects=(await readFile(join(workspace,"effects.log"),"utf8")).trim().split("\n").map(Number);
    assert.deepEqual(effects,Array.from({length:count},(_,i)=>i));
    const frames=wire.filter(row=>row.direction==="broker"&&row.frame.type==="call");
    const results=wire.filter(row=>row.direction==="host"&&row.frame.type==="result");
    assert.equal(frames.length,count);assert.equal(results.length,count);
    const diagnostics=await request(`/v1/agents/${thread}/diagnostics?limit=1024`);assert.equal(diagnostics.status,200);
    await writeFile(join(output,"diagnostics.json"),JSON.stringify(diagnostics.value,null,2));
    const history=await request(`/v1/agents/${thread}/events/history?after=0&limit=256`);assert.equal(history.status,200);
    await writeFile(join(output,"events.json"),JSON.stringify(history.value,null,2));
    // Stream writes can lag the public completion receipt by a tick.
    for(let i=0;i<100&&records.filter(row=>row.type==="managed.agent.tool"&&row.tool==="exec_command"&&row.message_type==="tool.result").length<count;i++) await delay(10);
    const nested=records.filter(row=>row.type==="managed.agent.tool"&&row.tool==="exec_command"&&row.message_type==="tool.result");
    const outer=records.filter(row=>row.type==="managed.agent.tool"&&row.tool==="exec"&&row.tool_call_id?.startsWith("call_warm_cell_")&&row.message_type==="tool.result");
    assert.equal(nested.length,count);assert.equal(outer.length,2);
    assert.equal(new Set(nested.map(row=>row.runtime_session_id)).size,1,"all calls stay in one running agent runtime");
    assert.ok(nested.every(row=>row.thread_id===thread&&row.turn_id===accepted.value.turn_id));
    const preparations=records.filter(row=>row.type==="hand.tool.stage"&&row.stage==="namespace.prepare");
    assert.equal(preparations.length,2,"exactly one initial capture per cell");
    const modelOutputs=records.filter(row=>row.type==="fixture.model").flatMap(row=>row.input??[]).filter(item=>item.type==="custom_tool_call_output"&&item.call_id?.startsWith("call_warm_cell_"));
    const emitted=modelOutputs.flatMap(item=>item.output??[]).filter(item=>item.type==="input_text"&&item.text.startsWith("{")).map(item=>JSON.parse(item.text));
    const guestSamples=emitted.filter(row=>Number.isInteger(row.index));
    const catalogCounts=emitted.filter(row=>Number.isInteger(row.catalog_count));
    assert.equal(guestSamples.length,count); assert.equal(catalogCounts.length,2);
    assert.ok(catalogCounts.every(row=>row.catalog_count>=50),"actual managed static catalog must remain present");
    const discovery=records.find(row=>row.type==="managed.agent.tool"&&row.tool==="tool_search"&&row.message_type==="tool.result");
    assert.equal(discovery?.outcome,"success");
    assert.ok(nested.every(row=>row.model_call_index>discovery.model_call_index));
    const sourceIds=[];
    const samples=frames.map((row,index)=>{
      const broker=records.find(entry=>entry.type==="hand.call.broker"&&entry.transport_call_id===row.frame.call_id&&entry.stage==="receipt");
      const id=broker?.source_call_id; sourceIds.push(id);
      const lifecycle=nested.find(entry=>entry.tool_call_id===id);
      const provider=records.find(entry=>entry.type==="hand.call.provider"&&entry.source_call_id===id);
      const account=records.find(entry=>entry.type==="hand.call.account"&&entry.source_call_id===id);
      const reply=results.find(entry=>entry.frame.call_id===row.frame.call_id);
      const stages=records.filter(entry=>entry.type==="hand.tool.stage"&&entry.call_id===id);
      assert.equal(reply?.frame.outcome.output.structured_result.output,`WARM_OK_${index}`);
      assert.equal(reply?.frame.outcome.output.structured_result.exit_code,0);
      assert.ok(lifecycle&&provider&&account&&broker&&reply,`missing correlated timing ${index}: ${JSON.stringify({id,lifecycle,provider,account,broker})}`);
      const guest=guestSamples.find(entry=>entry.index===index); assert.ok(guest);
      assert.equal(lifecycle.outcome,"success"); assert.equal(provider.outcome,"ok"); assert.equal(account.outcome,"ok"); assert.equal(broker.outcome,"completed");
      assert.ok([lifecycle,provider,account,broker].every(entry=>entry.thread_id===thread));
      return {index,guest_elapsed_ms:guest.elapsed_ms,cell:Math.floor(index/12),first_capture_in_cell:index%12===0,source_call_id:id,transport_call_id:row.frame.call_id,lifecycle,provider,account,broker,host_timing:reply.frame.timing,wire_elapsed_ms:reply.at_ms-row.at_ms,stages};
    });
    assert.equal(new Set(sourceIds).size,count);
    result.observed={once_only_effects:effects.length,native_calls:frames.length,warm_calls:samples.slice(1).length,code_mode_cells:outer.length,runtime_sessions:1,namespace_captures:preparations,catalogCounts,discovery,first_call:samples[0],second_cell_first_call:samples[12],public_turn_ms:publicTurnMs,samples,outer};
    const fields={guest_await_ms:s=>s.guest_elapsed_ms,nested_ms:s=>s.lifecycle.duration_ms,provider_ms:s=>s.provider.total_ms,provider_fetch_ms:s=>s.provider.fetch_ms,account_ms:s=>s.account.total_ms,account_handler_ms:s=>s.account.handler_ms,broker_roundtrip_ms:s=>s.broker.roundtrip_ms,native_execution_ms:s=>s.host_timing.execution_ms,host_elapsed_ms:s=>s.host_timing.host_elapsed_ms,wire_elapsed_ms:s=>s.wire_elapsed_ms};
    result.observed.summary=Object.fromEntries(Object.entries(fields).map(([field,get])=>[field,{all:summarize(samples.map(get)),after_first:summarize(samples.slice(1).map(get)),steady_no_capture:summarize(samples.filter(s=>!s.first_capture_in_cell).map(get))}]));
    for(const sample of samples) assert.ok(Object.values(fields).every(get=>Number.isFinite(get(sample))),JSON.stringify(sample));
    // Actual native observability, not fabricated stopwatch telemetry.
    const response=await fetch(new URL("/cdn-cgi/explorer/api/local/observability/query",base),{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({sql:"SELECT trace_id,span_id,parent_id,service,name,duration_ms,json(attributes) AS attributes FROM spans ORDER BY start_ms,span_id",params:[]})});
    assert.equal(response.status,200);const captured=await response.json();
    assert.equal(captured.success,true);
    const spans=captured.result.rows.map(row=>Object.fromEntries(captured.result.columns.map((column,index)=>[column,row[index]])));
    await writeFile(join(output,"spans.json"),JSON.stringify(captured,null,2));
    for(const sample of samples) {
      const span=spans.find(span=>span.name==="hand.account.invoke"&&JSON.parse(span.attributes)["nanocodex.tool_call_id"]===sample.source_call_id);
      assert.ok(span,`missing real native account span for ${sample.source_call_id}`);
      const attributes=JSON.parse(span.attributes);
      assert.equal(attributes["nanocodex.thread_id"],thread);
      assert.equal(attributes["nanocodex.transport_call_id"],sample.transport_call_id);
      assert.equal(attributes["nanocodex.connection_id"],sample.broker.connection_id);
      sample.account_span_ms=span.duration_ms;
    }
    console.log(JSON.stringify({evidence:output,label,router_sha256:hash(routerBytes),public_turn_ms:publicTurnMs,summary:result.observed.summary}));
  } catch(error) {failure=error;result.error=error.stack;throw error;}
  finally {try{await attachment?.close();await tools?.close();await native?.close();await mf?.dispose();}finally{
    await writeFile(join(output,"results.json"),JSON.stringify(result,null,2));await writeFile(join(output,"records.json"),JSON.stringify(records,null,2));await writeFile(join(output,"wire.json"),JSON.stringify(wire,null,2));await writeFile(join(output,"http.json"),JSON.stringify(http,null,2));await writeFile(join(output,"runtime.log"),runtime.join("\n")+"\n");
    await writeFile(join(output,"progress.json"),JSON.stringify({status:failure?"failed":"passed",error:failure?.message},null,2));
    await writeFile(join(output,"README.md"),`Run: \`${command}\`\nLabel: ${label}\nRouter source: ${selectedRouter}\nStatus: ${failure?"FAIL: "+failure.message:"PASS"}\n\n24 sequential native exec calls, 12 per each of two Code Mode cells, same running thread and turn after tool_search discovery. Direct within-cell Date.now awaited-call timings accompany managed nested/tool/provider/account/broker timing and real native account spans. First call (including first namespace capture) and second cell's first capture are separate samples; 23 after-first and 22 no-capture samples. No artificial delays, no WAN or hosted production-latency claim. Actual local workerd/WASM/SQLite/HTTP/WebSocket and native /bin/sh. Synthetic external model/auth seed only. All production static tool definitions remain available. Managed router exactly aliases CODE_MODE_WARM_SOURCE or current source; host-side native Tools always loads current production source. source-resolution.json records hashes and bundle inputs. effects.log, events.json, diagnostics.json, wire.json, records.json, spans.json, runtime.log, source, worker and SQLite are retained, including failures and outliers. No journal removals.\n`);
  }}
});
