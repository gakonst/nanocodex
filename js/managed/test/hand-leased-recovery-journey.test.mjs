import assert from "node:assert/strict";
import { fork } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";
import { fetch } from "./support/miniflare-fetch.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const machine = "synthetic-leased-hand", credential = "Bearer synthetic-leased-admission";
const command = "pnpm --filter nanocodex-managed-service exec node --test test/hand-leased-recovery-journey.test.mjs";
// The external VM authority has a short fixture lease, renewed independently of
// the tool socket. Broker, SQL, HTTP, WS, runtime journal and shell remain real.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { HostedToolsBroker } from './src/hosted-tools-broker.ts';
export class FixtureBroker extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.broker = new HostedToolsBroker(ctx, {renewLeasedAttachment: async scope => {
      const response = await fetch(env.AUTHORITY + '/validate', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify(scope)});
      return response.ok ? (await response.json()).expires_at : undefined;
    }});
  }
  async fetch(request) {
    if (request.headers.get('authorization') !== '${credential}') return new Response(null, {status:401});
    const path = new URL(request.url).pathname;
    if (path === '/attach') return this.broker.upgrade('synthetic-vm-session', undefined, undefined, undefined, {
      expectedAttachmentId:'${machine}', fixedRouteId:'vm-host:synthetic-allocation:1',
      renewalToken:'synthetic-vm-scope-token', maximumLeaseExpiresAt:Date.now() + 500,
    });
    if (path === '/invoke') {
      const {name = 'exec_command', call_id, input} = await request.json();
      const tool = this.broker.machineTool('${machine}', name);
      if (!tool) return Response.json({status:'unavailable'}, {status:503});
      return Response.json(await tool.handler(input, {sessionId:'synthetic-caller', callId:call_id, model:'synthetic-model'}));
    }
    if (path === '/inspect') return Response.json({online:this.broker.machineOnline('${machine}'),
      routes:this.ctx.storage.sql.exec('SELECT * FROM hosted_tool_routes').toArray(),
      calls:this.ctx.storage.sql.exec('SELECT * FROM hosted_tool_calls ORDER BY created_at,source_call_id').toArray()});
    return new Response(null, {status:404});
  }
  webSocketMessage(socket, message) { return this.broker.webSocketMessage(socket, message); }
  webSocketClose(socket, code, reason) { this.broker.webSocketClose(socket, code, reason); }
  webSocketError(socket) { this.broker.webSocketError(socket); }
}
export default {fetch(request, env) { return env.BROKER.getByName('synthetic-vm-broker').fetch(request); }};
`;
const childSource = `
import { Miniflare } from ${JSON.stringify(import.meta.resolve("miniflare"))};
import { readFile } from 'node:fs/promises';
try {
  const directory = process.argv[2];
  const mf = new Miniflare({port:Number(process.argv[3]), modules:true, script:await readFile(directory+'/worker.mjs','utf8'),
    compatibilityDate:'2026-07-30', compatibilityFlags:['nodejs_compat','enable_request_signal'],
    bindings:{AUTHORITY:process.argv[4]}, durableObjects:{BROKER:{className:'FixtureBroker',useSQLite:true}},
    durableObjectsPersist:directory+'/sqlite', handleRuntimeStdio(stdout,stderr) {stdout.pipe(process.stdout);stderr.pipe(process.stderr);}});
  process.send({base:(await mf.ready).href});
} catch(error) {process.send({error:error.stack});process.exitCode=1;}
`;

test("leased Hand validates cached expiry, survives SQLite restart, and keeps deadlines and revocation authoritative", {timeout:45000}, async () => {
  const output = join(root, "../../output/hand-leased-recovery-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand"); await mkdir(workspace, {recursive:true});
  const wire = [], trace = [], runtime = [], authorityTrace = [], sockets = [], authoritySockets = new Set();
  const evidence = {command, inputs:{machine,shell:"/bin/sh",authority_lease_ms:500,restart:"SIGKILL and same-port SQLite"},
    expected:{quiet_poll:"QUIET_POLL_OK",recovery:"SQL_RECOVERED",effects:"Q/R/H/V once each",deadline_independent:true,revoked_replay:false,json_heartbeats:0},observed:{}};
  let child, base, port = 0, gate, connector, tools, native, failure;
  let mode = "valid", authorityExpiresAt = Date.now() + 500;
  const renew = setInterval(() => { if (mode === "valid" || mode === "hang") authorityExpiresAt = Date.now() + 500; }, 50);
  const authority = createServer(async (request, response) => {
    let encoded = ""; for await (const chunk of request) encoded += chunk;
    const scope = JSON.parse(encoded);
    authorityTrace.push({at:Date.now(),mode,scope,expires_at:authorityExpiresAt});
    if (mode === "hang") return;
    if (mode === "revoked" || scope.renewalToken !== "synthetic-vm-scope-token"
      || scope.fixedRouteId !== "vm-host:synthetic-allocation:1" || scope.expectedAttachmentId !== machine) {
      response.writeHead(404); response.end(); return;
    }
    response.setHeader("content-type", "application/json"); response.end(JSON.stringify({expires_at:authorityExpiresAt}));
  });
  authority.on("connection", socket => {authoritySockets.add(socket);socket.on("close",()=>authoritySockets.delete(socket));});
  await new Promise(resolve => authority.listen(0,"127.0.0.1",resolve));
  const authorityOrigin = `http://127.0.0.1:${authority.address().port}`;
  const bounded = async (promise, description, ms = 8000) => {
    const abort = new AbortController();
    try {return await Promise.race([promise,delay(ms,undefined,{signal:abort.signal}).then(()=>{throw Error(`${description} exceeded ${ms}ms`);})]);}
    finally {abort.abort();}
  };
  const waitFor = async (predicate, description) => {
    const until = performance.now()+8000;
    while(performance.now()<until) {const value=await predicate();if(value)return value;await delay(15);}
    throw Error(description);
  };
  const start = async () => {
    const current = fork(join(output,"runtime-process.mjs"),[output,String(port),authorityOrigin],{detached:true,stdio:["ignore","pipe","pipe","ipc"]}); child=current;
    current.stdout.on("data",chunk=>runtime.push({pid:current.pid,text:String(chunk)}));
    current.stderr.on("data",chunk=>runtime.push({pid:current.pid,text:String(chunk)}));
    const ready=await bounded(new Promise((resolve,reject)=>{current.once("error",reject);current.once("exit",code=>reject(Error(`runtime exited ${code}`)));current.once("message",value=>value.error?reject(Error(value.error)):resolve(value));}),"runtime start");
    base=new URL(ready.base);if(port)assert.equal(Number(base.port),port);port=Number(base.port);trace.push({phase:"runtime_ready",pid:current.pid,base:base.href});
  };
  const kill = async () => {
    if(!child)return;const current=child;child=undefined;
    const exited=new Promise(resolve=>current.once("exit",(code,signal)=>{trace.push({phase:"runtime_exit",pid:current.pid,code,signal});resolve(signal);}));
    process.kill(-current.pid,"SIGKILL");assert.equal(await bounded(exited,"runtime kill"),"SIGKILL");
  };
  const request = async (path,body) => {
    const response=await fetch(new URL(path,base),{method:body===undefined?"GET":"POST",headers:{authorization:credential,"content-type":"application/json"},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(10000)});
    return {status:response.status,value:await response.json()};
  };
  const api = async (path,body) => {const result=await request(path,body);assert.equal(result.status,200,JSON.stringify(result));return result.value;};
  const inspect=()=>api("/inspect");
  const invoke=(call_id,input,name)=>api("/invoke",{call_id,input,name});
  const shell=cmd=>({cmd,shell:"/bin/sh",login:false,yield_time_ms:30000});
  const file=async name=>{try{return await readFile(join(workspace,name),"utf8");}catch(error){if(error.code!=="ENOENT")throw error;}};
  const calls=()=>wire.filter(row=>row.direction==="broker"&&row.frame.type==="call");
  try {
    const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,metafile:true,format:"esm",platform:"node",target:"es2022",external:["cloudflare:*","node:*"],logLevel:"warning"});
    await writeFile(join(output,"fixture-source.mjs"),source);await writeFile(join(output,"worker.mjs"),bundle.outputFiles[0].text);await writeFile(join(output,"runtime-process.mjs"),childSource);
    const candidateRoot=fileURLToPath(new URL("../../../",import.meta.url));
    const resolutions=Object.fromEntries(["nanocodex/tools","nanocodex-tools/attachment","nanocodex-tools/node","nanocodex-tools/hosted"].map(name=>[name,fileURLToPath(import.meta.resolve(name))]));
    for(const path of Object.values(resolutions))assert.ok(path.startsWith(candidateRoot),`dependency escaped candidate: ${path}`);
    await writeFile(join(output,"source-resolution.json"),JSON.stringify({resolutions,inputs:Object.keys(bundle.metafile.inputs)},null,2));
    await start();
    native=await createNodeProcessTools({workspace});
    tools=await createTools({tools:native.tools.map(tool=>({...tool,timeoutMs:tool.name==="exec_command"?8000:2500}))});
    connector=createAttachment(tools,{endpoint:new URL("/attach",base).href.replace(/^http/,"ws"),transport:{async connect(endpoint) {
      if(gate)await gate;
      const attempt=sockets.length+1,socket=new WebSocket(endpoint,{headers:{authorization:credential}});sockets.push(socket);
      const send=socket.send.bind(socket);socket.send=(data,...args)=>{wire.push({at:Date.now(),attempt,direction:"host",frame:JSON.parse(String(data))});return send(data,...args);};
      socket.on("message",data=>wire.push({at:Date.now(),attempt,direction:"broker",frame:JSON.parse(String(data))}));
      socket.on("pong",data=>wire.push({at:Date.now(),attempt,event:"control_pong",bytes:data.length}));
      socket.on("error",error=>wire.push({at:Date.now(),attempt,event:"error",error:error.message}));
      socket.on("close",(code,reason)=>wire.push({at:Date.now(),attempt,event:"close",code,reason:String(reason)}));return socket;
    }}},{machines:[{id:machine,name:"Synthetic leased VM",workspace,capabilities:["shell"]}],attachmentId:machine,heartbeatMs:50,reconnectDelayMs:20,drainTimeoutMs:500});
    const client=await connector.connect();
    const running=await invoke("start-quiet",{...shell("printf Q >> quiet.log; sleep 1.4; printf QUIET_POLL_OK"),yield_time_ms:1});
    const processId=running.structuredResult.session_id;assert.ok(processId);
    const beforePoll=await inspect(), authorityCount=authorityTrace.length;
    const polled=await invoke("quiet-poll",{session_id:processId,chars:"",yield_time_ms:2500},"write_stdin");
    assert.equal(polled.success,true);assert.equal(polled.structuredResult.output,"QUIET_POLL_OK");
    assert.equal(await file("quiet.log"),"Q");assert.equal(client.connected,true);
    assert.ok(authorityTrace.length>=authorityCount+2,"quiet poll must refresh authority past initial cached expiry");
    trace.push({phase:"quiet_poll",beforePoll,polled,after:await inspect()});

    const recoveryInput=shell("printf R >> recovery.log; while [ ! -f release-recovery ]; do sleep 0.02; done; printf SQL_RECOVERED");
    const original=invoke("recover-original",recoveryInput).then(value=>({value}),error=>({error:error.message}));
    await waitFor(async()=>await file("recovery.log")==="R","original shell effect");
    const beforeKill=await inspect(), oldState=beforeKill.routes[0];
    let release;gate=new Promise(resolve=>{release=resolve;});await kill();
    assert.ok((await original).error);
    await delay(Math.max(0,oldState.lease_expires_at-Date.now())+80);
    await start();
    const afterRestart=await inspect();
    assert.equal(afterRestart.calls.find(row=>row.source_call_id==="recover-original").state,"dispatched","SQL must retain proof past cached expiry");
    assert.equal(afterRestart.calls.find(row=>row.source_call_id==="recover-original").deadline_at,
      beforeKill.calls.find(row=>row.source_call_id==="recover-original").deadline_at,"restart must retain original deadline");
    gate=undefined;release();await waitFor(()=>client.connected,"same living runtime reconnect");
    const resumed=await inspect();assert.equal(resumed.routes[0].generation,oldState.generation);assert.equal(resumed.routes[0].lease_id,oldState.lease_id);
    await writeFile(join(workspace,"release-recovery"),"release");
    const recovered=await invoke("recover-original",recoveryInput);assert.equal(recovered.success,true);assert.equal(recovered.structuredResult.output,"SQL_RECOVERED");
    assert.equal(await file("recovery.log"),"R");
    assert.equal(calls().filter(row=>row.frame.call_id===beforeKill.calls.find(row=>row.source_call_id==="recover-original").call_id).length,1);
    trace.push({phase:"sqlite_recovery",beforeKill,afterRestart,resumed,recovered});

    const deadlineProcess=await invoke("start-deadline",{...shell("printf H >> deadline.log; while [ ! -f release-deadline ]; do sleep 0.02; done; printf MUST_NOT_COMPLETE"),yield_time_ms:1});
    const hung=invoke("deadline-original",{session_id:deadlineProcess.structuredResult.session_id,chars:"",yield_time_ms:2500},"write_stdin");
    await waitFor(async()=>await file("deadline.log")==="H","deadline shell effect");
    const deadlineState=await waitFor(async()=>{const state=await inspect();return state.calls.some(row=>row.source_call_id==="deadline-original"&&row.state==="dispatched")?state:undefined;},"deadline poll dispatched");
    const deadlineRow=deadlineState.calls.find(row=>row.source_call_id==="deadline-original");mode="hang";
    await waitFor(()=>authorityTrace.some(row=>row.mode==="hang"),"stalled authority lookup");
    const deadlineResult=await bounded(hung,"original admitted deadline",3500);
    const deadlineSettledAt=Date.now();assert.equal(deadlineResult.structuredResult.status,"ambiguous");assert.ok(deadlineSettledAt<=deadlineRow.deadline_at+500);
    assert.equal(await file("deadline.log"),"H");
    // The delayed authority failure must be finite, and cannot dispatch again.
    await waitFor(()=>!client.connected,"finite authority timeout fence");
    trace.push({phase:"authority_stall_deadline",deadlineRow,deadlineResult,deadlineSettledAt,after:await inspect()});
    mode="valid";authorityExpiresAt=Date.now()+500;await waitFor(()=>client.connected,"reconnect after unavailable authority");

    const revoked=invoke("revoke-original",shell("printf V >> revoked.log; while [ ! -f release-revoked ]; do sleep 0.02; done; printf MUST_NOT_REPLAY"));
    await waitFor(async()=>await file("revoked.log")==="V","revocation shell effect");mode="revoked";
    const revokeResult=await bounded(revoked,"authoritative revocation");assert.equal(revokeResult.structuredResult.status,"ambiguous");
    await waitFor(()=>!client.connected,"revoked runtime fence");
    const callCount=calls().length, replay=await request("/invoke",{call_id:"revoke-original",input:shell("printf V >> revoked.log")});
    assert.equal(replay.status,503);assert.equal(calls().length,callCount);assert.equal(await file("revoked.log"),"V");
    assert.equal(wire.filter(row=>row.frame?.type==="ping"||row.frame?.type==="pong").length,0);assert.ok(wire.some(row=>row.event==="control_pong"));
    trace.push({phase:"authoritative_revocation",revokeResult,replay,after:await inspect()});
    evidence.observed={quiet_poll:polled.structuredResult.output,recovery:recovered.structuredResult.output,same_runtime_epoch:true,original_dispatches:1,deadline_independent:true,revoked_replay:false,json_heartbeats:0};
    console.log(JSON.stringify({evidence:output,...evidence.observed}));
  } catch(error) {failure=error;evidence.error=error.stack;throw error;}
  finally {
    gate=undefined;for(const socket of sockets)socket.terminate();
    await bounded(connector?.close()??Promise.resolve(),"connector cleanup",1500).catch(()=>{});
    await tools?.close();await native?.close();await kill();clearInterval(renew);
    for(const socket of authoritySockets)socket.destroy();await new Promise(resolve=>authority.close(resolve));
    await writeFile(join(output,"trace.json"),JSON.stringify({evidence,trace},null,2));await writeFile(join(output,"wire.json"),JSON.stringify(wire,null,2));
    await writeFile(join(output,"authority.json"),JSON.stringify(authorityTrace,null,2));await writeFile(join(output,"runtime.log"),runtime.map(row=>`[${row.pid}] ${row.text}`).join(""));
    await writeFile(join(output,"README.md"),`Command: ${command}\nInputs: ${JSON.stringify(evidence.inputs)}\nExpected: ${JSON.stringify(evidence.expected)}\nObserved: ${JSON.stringify(evidence.observed)}\nStatus: ${failure?"FAIL: "+failure.message:"PASS"}\nEvidence: trace.json, wire.json, authority.json, runtime.log, hand/*.log, SQLite, bundled worker and fixture source. Only the external VM authority is synthetic; no broker state or clock is fabricated.\n`);
  }
});
