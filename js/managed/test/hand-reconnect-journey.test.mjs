import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket, { WebSocketServer } from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";
import { createAttachment as createLegacyAttachment } from "./fixtures/hand-publisher-546bec456.mjs";
import { fetch } from "./support/miniflare-fetch.mjs";

// Real public JS publisher, ToolRouter, workerd WebSockets, broker, SQLite and
// native /bin/sh. Only admission credentials and clocks are fixture inputs.
// The HTTP wrapper calls the broker's public machine API and retains one old
// binding to exercise generation fencing without replacing broker behavior.
const root = fileURLToPath(new URL("..", import.meta.url));
const credential = "Bearer synthetic-hand-reconnect";
const machineId = "synthetic-hand";
const source = `
import { DurableObject } from 'cloudflare:workers';
import { HostedToolsBroker } from './src/hosted-tools-broker.ts';
import { beginHandTiming, finishHandTiming } from './src/hand-timing.ts';
import { routeManaged } from '../account/worker/managedProxy.ts';
export class FixtureBroker extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env); this.offset = 0; this.bindings = new Map(); this.cancellations = new Map();
    this.broker = new HostedToolsBroker(ctx, {resumeRetainedSockets: true, now: () => Date.now() + this.offset});
  }
  async fetch(request) {
    if (request.headers.get('authorization') !== '${credential}') return new Response(null, {status: 401});
    const path = new URL(request.url).pathname;
    if (path === '/attach') return this.broker.upgrade('synthetic-session');
    if (path === '/clock') { this.offset = (await request.json()).offset; return Response.json({offset: this.offset}); }
    if (path === '/bind') {
      const {id} = await request.json();
      const tool = this.broker.machineTool('${machineId}', 'exec_command');
      if (!tool) return new Response(null, {status: 503});
      this.bindings.set(id, tool); return Response.json({id, route_token: tool.routeToken});
    }
    if (path === '/cancel') { const {call_id} = await request.json(); this.cancellations.get(call_id)?.abort(); return Response.json({cancelled:true}); }
    if (path === '/invoke') {
      const {binding, call_id, input} = await request.json();
      const tool = binding ? this.bindings.get(binding) : this.broker.machineTool('${machineId}', 'exec_command');
      if (!tool) return new Response(null, {status: 503});
      const controller = new AbortController(); this.cancellations.set(call_id, controller);
      return Response.json(await tool.handler(input, {sessionId: 'synthetic-session',
        callId: call_id, parentCallId: '', model: 'synthetic-model', signal: controller.signal}));
    }
    if (path === '/inspect') return Response.json({now: Date.now() + this.offset,
      online: this.broker.machineOnline('${machineId}'), machines: this.broker.machines(),
      routes: this.ctx.storage.sql.exec('SELECT route_id,generation,lease_id,lease_expires_at,runtime_id,command_recovery FROM hosted_tool_routes').toArray(),
      calls: this.ctx.storage.sql.exec('SELECT call_id,source_call_id,generation,lease_id,state,dispatched_at,result_json,receipt_json FROM hosted_tool_calls ORDER BY created_at,source_call_id').toArray()});
    return new Response(null, {status: 404});
  }
  webSocketMessage(socket, message) { return this.broker.webSocketMessage(socket, message); }
  webSocketClose(socket, code, reason) { this.broker.webSocketClose(socket, code, reason); }
  webSocketError(socket) { this.broker.webSocketError(socket); }
}
export default {fetch(request, env) {
  const url = new URL(request.url);
  if (url.pathname === '/v1/account/tool-host') {
    // Use the shipped account forwarding and managed response wrapper around
    // the real broker upgrade. Only authentication/routing are fixture inputs.
    return routeManaged(request, {NANOCODEX_BACKEND: {async fetch(forwarded) {
      beginHandTiming(forwarded);
      const target = new URL(forwarded.url); target.pathname = '/attach';
      const response = await env.BROKER.getByName('synthetic-broker').fetch(new Request(target, forwarded));
      return finishHandTiming(forwarded, response);
    }}}, url);
  }
  return env.BROKER.getByName('synthetic-broker').fetch(request);
}};
`;


test("a living Hand recovers offline execution, lost ACKs, cancellation, and fences replacement receipts", {timeout:45_000}, async () => {
  const output = join(root, "../../output/hand-command-recovery", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, {recursive:true});
  const wire = [], trace = [], runtime = [], nativeResults = [], connectors = [], sockets = [];
  const result = {command:"pnpm --filter nanocodex-managed-service test:hand-reconnect",
    inputs:{shell:"/bin/sh", transport_loss:"terminate", dropped_ack:true, lost_call_before_delivery:true, ping_interval_ms:250},
    expected:{offline_output:"OFFLINE_OK", effect_count:1, offline_call_frames:1, lost_ack_call_frames:1,
      same_lease:true, same_generation:true, missing_journal_status:"ambiguous", missing_journal_host_state:"missing", missing_journal_call_frames:1, missing_journal_effect_count:0, offline_cancel:"ambiguous", replacement:"ambiguous", stale_close:1008, json_heartbeats:0,
      managed_upgrade_timing:true}, observed:{}};
  const bundle = await build({stdin:{contents:source, resolveDir:root}, bundle:true, write:false,
    metafile:true, format:"esm", platform:"node", target:"es2022", external:["cloudflare:*", "node:*"], logLevel:"warning"});
  const candidateRoot = fileURLToPath(new URL("../../../", import.meta.url));
  const resolutions = Object.fromEntries(["nanocodex/tools", "nanocodex-tools/attachment", "nanocodex-tools/node", "nanocodex-tools/runtime/tool-router", "nanocodex-tools/hosted"]
    .map(name => [name, fileURLToPath(import.meta.resolve(name))]));
  for (const path of Object.values(resolutions)) assert.ok(path.startsWith(candidateRoot), `dependency escaped checkout: ${path}`);
  await writeFile(join(output,"source-resolution.json"),JSON.stringify({resolutions, bundleInputs:Object.keys(bundle.metafile.inputs)},null,2));
  await writeFile(join(output,"fixture-source.mjs"),source);
  await writeFile(join(output,"worker.mjs"),bundle.outputFiles[0].text);
  const mf = new Miniflare({port:0, modules:true, script:bundle.outputFiles[0].text,
    compatibilityDate:"2026-07-30", compatibilityFlags:["nodejs_compat", "enable_request_signal"],
    durableObjects:{BROKER:{className:"FixtureBroker",useSQLite:true}},durableObjectsPersist:join(output,"sqlite"),
    handleRuntimeStdio(stdout,stderr) {
      createInterface({input:stdout}).on("line",line => runtime.push(line));
      createInterface({input:stderr}).on("line",line => runtime.push(line));
    }});
  const base = await mf.ready, endpoint = new URL("/v1/account/tool-host",base); endpoint.protocol="ws:";
  const api = async (path,body) => {
    const response = await fetch(new URL(path,base),{method:body === undefined ? "GET":"POST",
      headers:{authorization:credential,"content-type":"application/json"}, body:body === undefined ? undefined:JSON.stringify(body)});
    const value = await response.json(); assert.equal(response.status,200,JSON.stringify({path,value})); return value;
  };
  const inspect = () => api("/inspect");
  const waitFor = async (predicate,description) => {
    const deadline = performance.now()+8000;
    while (performance.now()<deadline) { const value=await predicate(); if(value)return value; await delay(10); }
    assert.fail(`${description}: ${JSON.stringify({wire,inspection:await inspect()})}`);
  };
  const file = async name => { try {return await readFile(join(workspace,name),"utf8");} catch(error) {if(error.code!=="ENOENT")throw error;} };
  const native = await createNodeProcessTools({workspace,onActivity:event=>trace.push({native:event})});
  const tools = await createTools({tools:native.tools.map(tool => tool.name !== "exec_command" ? tool : {
    ...tool,async handler(input,context) {const value=await tool.handler(input,context);nativeResults.push({call_id:context.callId,value});return value;}
  })});
  let reconnectGate, dropAck=false, dropNextCall=false, failure;
  const publish = label => {
    const connector = createAttachment(tools,{endpoint:endpoint.href,transport:{async connect() {
      if(reconnectGate) await reconnectGate;
      const attempt=wire.filter(row=>row.publisher===label&&row.event==="connect").length+1;
      const socket=new WebSocket(endpoint,{headers:{authorization:credential}});sockets.push(socket);
      wire.push({publisher:label,attempt,event:"connect"});
      socket.on("upgrade",response=>wire.push({publisher:label,attempt,event:"upgrade",status:response.statusCode,
        server_timing:response.headers["server-timing"]}));
      const send=socket.send.bind(socket);
      socket.send=(data,...args)=>{wire.push({publisher:label,attempt,direction:"host",frame:JSON.parse(String(data))});return send(data,...args);};
      // Fault injection drops only transport frames; executor and broker run unchanged.
      const emit=socket.emit.bind(socket);
      socket.emit=(event,...args)=>{
        if(event==="message") {
          const frame=JSON.parse(String(args[0]));wire.push({publisher:label,attempt,direction:"broker",frame});
          if(dropAck&&frame.type==="ack") {wire.push({publisher:label,attempt,event:"ack_dropped",call_id:frame.call_id});return false;}
          if(dropNextCall&&frame.type==="call") {
            dropNextCall=false;
            wire.push({publisher:label,attempt,event:"ordinary_call_dropped",call_id:frame.call_id});
            socket.terminate();return false;
          }
        }
        return emit(event,...args);
      };
      socket.on("pong",bytes=>wire.push({publisher:label,attempt,event:"control_pong",bytes:bytes.length}));
      socket.on("close",(code,reason)=>wire.push({publisher:label,attempt,event:"close",code,reason:String(reason)}));
      return socket;
    }}},{machines:[{id:machineId,name:"Synthetic Hand",workspace,capabilities:["shell"]}],
      attachmentId:machineId,heartbeatMs:250,reconnectDelayMs:1,drainTimeoutMs:500});
    connectors.push(connector);return connector;
  };
  const invoke=(callId,input)=>api("/invoke",{call_id:callId,input});
  const input=cmd=>({cmd,shell:"/bin/sh",login:false,yield_time_ms:30000});
  const calls=callId=>wire.filter(row=>row.direction==="broker"&&row.frame.type==="call"&&(!callId||row.frame.call_id===callId));
  const holdAndDrop=()=>{let release;reconnectGate=new Promise(resolve=>{release=resolve;});sockets.at(-1).terminate();return ()=>{reconnectGate=undefined;release();};};
  try {
    const first=publish("first"),client=await first.connect();
    const initial=await inspect();assert.equal(initial.online,true);assert.equal(initial.routes[0].command_recovery,1);
    await waitFor(()=>wire.filter(row=>row.event==="control_pong").length>=3,"control pongs through managed forwarding");
    await api("/clock",{offset:61001});assert.equal((await inspect()).online,true,"ordinary runtime has no liveness TTL");
    await api("/clock",{offset:0});

    const offlineInput=input("printf E >> offline.log; while [ ! -f release-offline ]; do sleep 0.02; done; printf OFFLINE_OK");
    const pending=invoke("offline-once",offlineInput);
    await waitFor(async()=>await file("offline.log")==="E","shell effect before drop");
    const callId=calls().at(-1).frame.call_id,release=holdAndDrop();
    await waitFor(()=>!client.connected,"attachment detached");
    assert.equal((await inspect()).online,false);
    await writeFile(join(workspace,"release-offline"),"release");
    await waitFor(()=>nativeResults.some(row=>row.call_id===callId),"shell completed offline");
    release();
    const recovered=await pending;
    assert.equal(recovered.success,true);assert.equal(recovered.structuredResult.output,"OFFLINE_OK");assert.equal(recovered.structuredResult.exit_code,0);
    const resumed=await inspect();assert.equal(resumed.routes[0].lease_id,initial.routes[0].lease_id);assert.equal(resumed.routes[0].generation,initial.routes[0].generation);
    assert.equal(await file("offline.log"),"E");assert.equal(calls(callId).length,1);
    assert.ok(wire.some(row=>row.direction==="broker"&&row.frame.type==="recover"&&row.frame.call_ids.includes(callId)));
    assert.ok(wire.filter(row=>row.direction==="host"&&row.frame.type==="catalog").every(row=>row.frame.runtime_id===initial.routes[0].runtime_id));
    trace.push({initial,resumed,recovered});

    dropAck=true;
    const ackInput=input("printf A >> ack.log; printf ACK_OK");
    const ackResult=await invoke("ack-once",ackInput);assert.equal(ackResult.structuredResult.output,"ACK_OK");
    const ackCallId=calls().at(-1).frame.call_id;
    await waitFor(()=>wire.some(row=>row.event==="ack_dropped"&&row.call_id===ackCallId),"dropped durable ACK");
    const resumeAck=holdAndDrop();await waitFor(()=>!client.connected,"lost ACK socket drop");dropAck=false;resumeAck();
    await waitFor(()=>wire.filter(row=>row.direction==="host"&&row.frame.type==="result"&&row.frame.call_id===ackCallId).length>=2&&client.connected,"retained receipt replay");
    assert.equal((await invoke("ack-once",ackInput)).structuredResult.output,"ACK_OK");
    assert.equal(await file("ack.log"),"A");assert.equal(calls(ackCallId).length,1);

    // The broker dispatched this frame, but real transport loss prevents its
    // delivery to the executor. The same living runtime has no journal proof.
    dropNextCall=true;
    const missingInput=input("printf M >> missing.log; printf MISSING_MUST_NOT_EXECUTE");
    const missing=await invoke("missing-before-delivery",missingInput);
    assert.equal(missing.structuredResult.status,"ambiguous");
    const dropped=wire.find(row=>row.event==="ordinary_call_dropped"),missingCallId=dropped.call_id;
    const missingStatus=wire.find(row=>row.direction==="host"&&row.frame.type==="status"&&row.frame.call_id===missingCallId);
    assert.equal(missingStatus.frame.state,"missing");
    assert.ok(wire.some(row=>row.direction==="broker"&&row.frame.type==="recover"&&row.frame.call_ids.includes(missingCallId)));
    assert.equal(calls(missingCallId).length,1);assert.equal(await file("missing.log"),undefined);
    assert.equal((await invoke("missing-before-delivery",missingInput)).structuredResult.status,"ambiguous");
    assert.equal(calls(missingCallId).length,1);assert.equal(await file("missing.log"),undefined);
    const afterMissing=await inspect();assert.equal(afterMissing.routes[0].lease_id,initial.routes[0].lease_id);
    assert.equal(afterMissing.routes[0].generation,initial.routes[0].generation);
    assert.equal(afterMissing.calls.find(row=>row.call_id===missingCallId).state,"ambiguous");
    trace.push({missing,missingStatus,afterMissing});

    const cancelPending=invoke("cancel-offline",input("printf C >> cancel.log; sleep 20; printf CANCEL_MUST_NOT_COMPLETE"));
    await waitFor(async()=>await file("cancel.log")==="C","cancellable shell effect");
    const cancelCallId=calls().at(-1).frame.call_id,resumeCancel=holdAndDrop();await waitFor(()=>!client.connected,"offline before cancellation");
    await api("/cancel",{call_id:"cancel-offline"});resumeCancel();
    const cancelled=await cancelPending;assert.equal(cancelled.structuredResult.status,"ambiguous");
    await waitFor(()=>wire.some(row=>row.direction==="broker"&&row.frame.type==="cancel"&&row.frame.call_id===cancelCallId),"cancel after recover");
    assert.equal(calls(cancelCallId).length,1);trace.push({cancelled});

    const replacementInput=input("printf N >> replaced.log; while [ ! -f release-replaced ]; do sleep 0.02; done; printf OLD_RESULT");
    const replacedPending=invoke("replaced-once",replacementInput);
    await waitFor(async()=>await file("replaced.log")==="N","replacement command started");
    const staleCallId=calls().at(-1).frame.call_id;
    const next=publish("replacement"),nextClient=await next.connect();
    const replaced=await replacedPending;assert.equal(replaced.structuredResult.status,"ambiguous");
    await waitFor(()=>!client.connected,"old runtime fenced");
    assert.ok(wire.some(row=>row.publisher==="first"&&row.event==="close"&&row.code===1008));
    await writeFile(join(workspace,"release-replaced"),"release");
    assert.equal((await invoke("replaced-once",replacementInput)).structuredResult.status,"ambiguous");
    assert.equal(await file("replaced.log"),"N");assert.equal(calls(staleCallId).length,1);
    const beforeStale=(await inspect()).calls.find(row=>row.call_id===staleCallId);
    // A successor runtime cannot submit an old runtime's terminal identity.
    sockets.at(-1).send(JSON.stringify({type:"result",call_id:staleCallId,outcome:{status:"completed",output:{output:"STALE",success:true,structured_result:null,metadata:null,process_trace:null}}}));
    await waitFor(()=>!nextClient.connected,"stale receipt fencing");
    assert.ok(wire.some(row=>row.publisher==="replacement"&&row.event==="close"&&row.code===1008));
    const final=await inspect(),staleRow=final.calls.find(row=>row.call_id===staleCallId);
    assert.equal(staleRow.state,"ambiguous");assert.equal(staleRow.result_json,beforeStale.result_json);assert.equal(staleRow.receipt_json,beforeStale.receipt_json);
    // A retained receipt with a lost ACK cannot keep detach waiting forever.
    const detach=publish("detach");await detach.connect();dropAck=true;
    const detachResult=await invoke("detach-no-ack",input("printf D >> detach.log; printf DETACH_OK"));
    assert.equal(detachResult.structuredResult.output,"DETACH_OK");
    const detachCallId=calls().at(-1).frame.call_id;
    await waitFor(()=>wire.some(row=>row.event==="ack_dropped"&&row.call_id===detachCallId),"ACK withheld before detach");
    const detachAt=performance.now();await detach.close();const detachMs=performance.now()-detachAt;
    assert.ok(detachMs<1500,`detach exceeded configured 500ms drain bound: ${detachMs}`);
    assert.equal(await file("detach.log"),"D");dropAck=false;
    const jsonHeartbeats=wire.filter(row=>row.frame&&["ping","pong"].includes(row.frame.type)).length;assert.equal(jsonHeartbeats,0);
    const upgrades=wire.filter(row=>row.event==="upgrade");assert.ok(upgrades.length>=4);
    for(const upgrade of upgrades) {
      assert.equal(upgrade.status,101);
      assert.match(upgrade.server_timing,/hand_total;/,"managed upgrade response wrapper did not run");
    }
    result.observed={offline_output:recovered.structuredResult.output,effect_count:1,offline_call_frames:calls(callId).length,
      lost_ack_call_frames:calls(ackCallId).length,missing_journal_status:missing.structuredResult.status,missing_journal_host_state:missingStatus.frame.state,missing_journal_call_frames:calls(missingCallId).length,missing_journal_effect_count:0,same_lease:true,same_generation:true,offline_cancel:cancelled.structuredResult.status,
      replacement:replaced.structuredResult.status,stale_close:1008,detach_without_ack_ms:detachMs,json_heartbeats:jsonHeartbeats,control_pongs:wire.filter(row=>row.event==="control_pong").length,
      managed_upgrade_timing:true,verified_upgrades:upgrades.length};
    trace.push({final});console.log(JSON.stringify({evidence:output,...result.observed}));
  }catch(error){failure=error;result.error=error.stack;throw error;}
  finally {
    await Promise.all(connectors.map(connector=>connector.close()));
    for(const socket of sockets)socket.terminate();
    await tools.close();await native.close();await mf.dispose();
    await writeFile(join(output,"trace.json"),JSON.stringify({result,trace},null,2));
    await writeFile(join(output,"wire.json"),JSON.stringify(wire,null,2));
    await writeFile(join(output,"native-results.json"),JSON.stringify(nativeResults,null,2));
    await writeFile(join(output,"runtime.log"),runtime.join("\n"));
    await writeFile(join(output,"README.md"),`Command: ${result.command}\nInputs: ${JSON.stringify(result.inputs)}\nExpected: ${JSON.stringify(result.expected)}\nObserved: ${JSON.stringify(result.observed)}\nStatus: ${failure?failure.stack:"PASS"}\nEvidence: trace.json, wire.json, native-results.json, hand/*.log, sqlite/, runtime.log, source-resolution.json. Journal proof survives only in this living executor; shell exactly-once across executor crashes is outside this protocol.\n`);
  }
});

// A real peer that withholds control pongs proves bounded liveness without
// relying on Cloudflare application-message handlers. Node's global standard
// WebSocket proves the public default does not synthesize JSON heartbeats.
test("control pong loss reconnects finitely and browser-standard sockets send no JSON heartbeat",{timeout:15000},async()=>{
  const output=join(root,"../../output/hand-control-ping",`${Date.now()}-${process.pid}`);await mkdir(output,{recursive:true});
  const server=new WebSocketServer({port:0,autoPong:false});
  await new Promise(resolve=>server.once("listening",resolve));
  const endpoint=`ws://127.0.0.1:${server.address().port}`,wire=[],peers=[];
  server.on("connection",peer=>{
    peers.push(peer);const attempt=peers.length;
    peer.on("ping",data=>wire.push({attempt,event:"control_ping",bytes:data.length}));
    peer.on("message",data=>{const frame=JSON.parse(String(data));wire.push({attempt,frame});if(frame.type==="catalog")peer.send(JSON.stringify({type:"ready"}));if(frame.type==="drain")peer.send(JSON.stringify({type:"draining"}));});
    peer.on("close",code=>wire.push({attempt,event:"close",code}));
  });
  const waitFor=async predicate=>{for(let i=0;i<300;i++){if(predicate())return;await delay(10);}assert.fail(JSON.stringify(wire));};
  const tools=await createTools();let connector,standard;
  try {
    connector=createAttachment(tools,{endpoint,transport:{connect:()=>new WebSocket(endpoint)}},{heartbeatMs:30,reconnectDelayMs:1});
    const client=await connector.connect();await waitFor(()=>peers.length>=3);
    assert.ok(wire.filter(row=>row.event==="close"&&row.code===1012).length>=2);
    await connector.close();assert.equal(client.connected,false);
    standard=createAttachment(tools,endpoint,{heartbeatMs:10,reconnectDelayMs:1});
    const standardClient=await standard.connect(),attempt=peers.length;
    await delay(80);assert.equal(standardClient.connected,true);
    assert.equal(wire.filter(row=>row.attempt===attempt&&row.event==="control_ping").length,0);
    assert.equal(wire.filter(row=>row.frame&&["ping","pong"].includes(row.frame.type)).length,0);
    peers.at(-1).terminate();await waitFor(()=>peers.length>attempt&&standardClient.connected);
    await standard.close();
    console.log(JSON.stringify({evidence:output,finite_control_timeout_closes:wire.filter(row=>row.event==="close"&&row.code===1012).length,standard_reconnected:true,json_heartbeats:0}));
  }finally {
    await connector?.close();await standard?.close();await tools.close();for(const peer of peers)peer.terminate();
    await new Promise(resolve=>server.close(resolve));
    await writeFile(join(output,"wire.json"),JSON.stringify(wire,null,2));
    await writeFile(join(output,"README.md"),"Run: pnpm --filter nanocodex-managed-service test:hand-reconnect\nExpected and observed: injected ws emits 16-byte control pings; withholding pong closes 1012 and reconnects within several 30ms intervals. Public global WebSocket remains connected over eight 10ms intervals, emits no JSON heartbeat, reconnects after real transport loss. wire.json captures frames/control events.\n");
  }
});

// The historical publisher is frozen independently of today's implementation:
// it sends JSON ping, accepts JSON pong and never advertises command_recovery.
test("a frozen legacy Hand keeps JSON heartbeats across hibernation and reconnect without recovery frames", {timeout:30_000}, async () => {
  const output = join(root, "../../output/hand-legacy-heartbeat", String(Date.now()) + "-" + process.pid);
  const workspace = join(output, "hand");
  await mkdir(workspace, {recursive:true});
  const wire = [], trace = [], runtime = [], sockets = [], connectors = [];
  const evidence = {command:"pnpm --filter nanocodex-managed-service test:hand-reconnect",
    inputs:{publisher_commit:"546bec45642622d718bdb7d23c42d276b21b388d", shell:"/bin/sh", heartbeat_ms:250, transport_loss:"terminate"},
    expected:{before:"LEGACY_BEFORE", parallel:"LEGACY_PARALLEL", pending:"LEGACY_PENDING", after:"LEGACY_AFTER", disconnected_status:"ambiguous", disconnected_effect_count:1, disconnected_call_frames:1, fresh_after_disconnect:"LEGACY_FRESH", recovered_commands:0, matched_pongs:true, invalid_frame_close:1008}, observed:{}};
  const bundle = await build({stdin:{contents:source, resolveDir:root}, bundle:true, write:false,
    metafile:true, format:"esm", platform:"node", target:"es2022", external:["cloudflare:*", "node:*"],
    alias:{"nanocodex-tools/hosted":fileURLToPath(new URL("../../nanocodex-tools/src/hosted/index.ts", import.meta.url))}, logLevel:"warning"});
  await writeFile(join(output,"worker.mjs"),bundle.outputFiles[0].text);
  await writeFile(join(output,"fixture-source.mjs"),source);
  await writeFile(join(output,"source-resolution.json"),JSON.stringify({bundleInputs:Object.keys(bundle.metafile.inputs),
    legacyPublisher:fileURLToPath(new URL("./fixtures/hand-publisher-546bec456.mjs",import.meta.url))},null,2));
  const mf = new Miniflare({name:"legacy-heartbeat",port:0,modules:true,script:bundle.outputFiles[0].text,
    compatibilityDate:"2026-07-30",compatibilityFlags:["nodejs_compat","enable_request_signal"],
    durableObjects:{BROKER:{className:"FixtureBroker",useSQLite:true}},durableObjectsPersist:join(output,"sqlite"),
    handleRuntimeStdio(stdout,stderr) {
      createInterface({input:stdout}).on("line",line=>runtime.push(line));
      createInterface({input:stderr}).on("line",line=>runtime.push(line));
    }});
  const base=await mf.ready, endpoint=new URL("/v1/account/tool-host",base);endpoint.protocol="ws:";
  const inspect=async()=>await(await fetch(new URL("/inspect",base),{headers:{authorization:credential}})).json();
  const waitFor=async(predicate,description)=>{
    const deadline=performance.now()+5000;
    while(performance.now()<deadline){const value=await predicate();if(value)return value;await delay(10);}
    assert.fail(description+": "+JSON.stringify({wire,inspection:await inspect()}));
  };
  const native=await createNodeProcessTools({workspace,onActivity:event=>trace.push({native:event})});
  const tools=await createTools({tools:native.tools.map(tool=>tool.name!=="exec_command"?tool:{
    ...tool,async handler(input,context) {
      try{return await tool.handler(input,context);}
      finally{trace.push({native_finished:context.callId});}
    },
  })});
  let connector, client, pending, disconnectedPending, failure;
  const pongs=attempt=>wire.filter(row=>row.attempt===attempt&&row.direction==="broker"&&row.frame?.type==="pong");
  const file=async name=>{try{return await readFile(join(workspace,name),"utf8");}catch(error){if(error.code!=="ENOENT")throw error;}};
  const invoke=async(callId,cmd)=>{
    const response=await fetch(new URL("/invoke",base),{method:"POST",headers:{authorization:credential,"content-type":"application/json"},
      body:JSON.stringify({call_id:callId,input:{cmd,shell:"/bin/sh",login:false,yield_time_ms:30000}})});
    const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));
    trace.push({call_id:callId,result:value});return value;
  };
  const connect=label=>{
    const value=createLegacyAttachment(tools,{endpoint:endpoint.href,transport:{connect(){
      const attempt=sockets.length+1,socket=new WebSocket(endpoint,{headers:{authorization:credential}});sockets.push(socket);
      const send=socket.send.bind(socket);
      socket.send=(data,...args)=>{wire.push({label,attempt,direction:"host",frame:JSON.parse(String(data))});return send(data,...args);};
      socket.on("message",data=>wire.push({label,attempt,direction:"broker",frame:JSON.parse(String(data))}));
      socket.on("close",(code,reason)=>wire.push({label,attempt,event:"close",code,reason:String(reason)}));
      return socket;
    }}},{machines:[{id:machineId,name:"Synthetic legacy Hand",workspace,capabilities:["shell"]}],
      attachmentId:machineId,heartbeatMs:250,reconnectDelayMs:1,drainTimeoutMs:500});
    connectors.push(value);return value;
  };
  try {
    connector=connect("legacy");client=await connector.connect();
    await waitFor(()=>pongs(1).length>=3,"legacy matching pongs before shell");
    const initial=await inspect();assert.equal(initial.online,true);trace.push({phase:"initial",inspection:initial});
    const before=await invoke("legacy-before","printf LEGACY_BEFORE");assert.equal(before.structuredResult.output,"LEGACY_BEFORE");
    // Wake the actual SQLite-backed DO through a legacy heartbeat after hibernation.
    const beforeHibernate=pongs(1).length;
    await mf.unsafeEvictDurableObject("legacy-heartbeat","FixtureBroker",{name:"synthetic-broker",webSockets:"hibernate"});
    await waitFor(()=>pongs(1).length>=beforeHibernate+2,"legacy heartbeat after hibernation");
    assert.equal(client.connected,true);
    sockets.at(-1).terminate();
    await waitFor(()=>sockets.length>=2&&client.connected,"legacy reconnect");
    await waitFor(()=>pongs(2).length>=3,"legacy pongs after reconnect");
    trace.push({phase:"reconnected",inspection:await inspect()});
    pending=invoke("legacy-pending","printf E >> effects.log; while [ ! -f release ]; do sleep 0.02; done; printf LEGACY_PENDING");
    await waitFor(async()=>await file("effects.log")==="E","native pending shell started");
    const pendingPongs=pongs(2).length;
    const parallel=await invoke("legacy-parallel","printf LEGACY_PARALLEL");assert.equal(parallel.structuredResult.output,"LEGACY_PARALLEL");
    await waitFor(()=>pongs(2).length>=pendingPongs+3,"legacy heartbeats while native command pending");
    await writeFile(join(workspace,"release"),"release");
    const completed=await pending;assert.equal(completed.structuredResult.output,"LEGACY_PENDING");assert.equal(await file("effects.log"),"E");
    const after=await invoke("legacy-after","printf LEGACY_AFTER");assert.equal(after.structuredResult.output,"LEGACY_AFTER");
    // A legacy runtime has no command-recovery journal. Lose transport after
    // the actual shell effect, preserve ambiguity, and never retry that input.
    const lostCommand="printf L >> disconnected.log; while [ ! -f release-disconnected ]; do sleep 0.02; done; printf LEGACY_LOST";
    disconnectedPending=invoke("legacy-disconnected",lostCommand);
    await waitFor(async()=>await file("disconnected.log")==="L","legacy effect before pending disconnect");
    const lostCall=wire.find(row=>row.direction==="broker"&&row.frame?.type==="call"&&row.frame.input.cmd===lostCommand);
    assert.ok(lostCall);const lostCallId=lostCall.frame.call_id;
    sockets.at(-1).terminate();
    const disconnected=await Promise.race([disconnectedPending,delay(5000,undefined,{ref:false}).then(()=>assert.fail("legacy pending caller did not settle after disconnect"))]);
    assert.equal(disconnected.structuredResult.status,"ambiguous");
    await waitFor(()=>sockets.length>=3&&client.connected,"legacy pending-call reconnect");
    await waitFor(()=>pongs(3).length>=3,"legacy heartbeats after pending-call reconnect");
    await writeFile(join(workspace,"release-disconnected"),"release");
    await waitFor(()=>trace.some(row=>row.native_finished===lostCallId),"disconnected native shell cleanup");
    const fresh=await invoke("legacy-fresh","printf LEGACY_FRESH");assert.equal(fresh.structuredResult.output,"LEGACY_FRESH");
    assert.equal(await file("disconnected.log"),"L");
    const lostFrames=wire.filter(row=>row.direction==="broker"&&row.frame?.type==="call"&&row.frame.call_id===lostCallId);
    assert.equal(lostFrames.length,1);trace.push({phase:"legacy_pending_disconnect",lost_call_id:lostCallId,disconnected,fresh,original_dispatches:lostFrames.length,effect_count:1});
    for(const row of wire.filter(row=>row.direction==="broker"&&row.frame?.type==="pong")) {
      assert.ok(wire.some(sent=>sent.attempt===row.attempt&&sent.direction==="host"&&sent.frame?.type==="ping"&&sent.frame.nonce===row.frame.nonce));
    }
    assert.ok(wire.filter(row=>row.frame?.type==="catalog").every(row=>row.frame.command_recovery===undefined));
    assert.equal(wire.filter(row=>row.frame?.type==="recover").length,0);
    await connector.close();
    // A valid heartbeat cannot claim a route before catalog admission.
    const unclaimed=new WebSocket(endpoint,{headers:{authorization:credential}});sockets.push(unclaimed);
    await new Promise((resolve,reject)=>{unclaimed.once("open",resolve);unclaimed.once("error",reject);});
    const unclaimedClose=new Promise(resolve=>unclaimed.once("close",(code,detail)=>resolve({code,reason:String(detail)})));
    unclaimed.send(JSON.stringify({type:"ping",nonce:"before-catalog"}));
    const unclaimedResult=await Promise.race([unclaimedClose,delay(3000,undefined,{ref:false}).then(()=>assert.fail("unclaimed heartbeat not fenced"))]);
    assert.equal(unclaimedResult.code,1008);assert.equal(unclaimedResult.reason.split(":")[0],"stale_socket");
    trace.push({invalid:"before-catalog",...unclaimedResult});
    // Exercise strict wire validation over real sockets, after valid legacy traffic.
    for(const [label,frame,reason] of [
      ["extra-field",{type:"ping",nonce:"synthetic",extra:true},"invalid_message"],
      ["oversized-nonce",{type:"ping",nonce:"\ud83d\udca3".repeat(33)},"invalid_string"],
      ["wrong-direction",{type:"pong",nonce:"synthetic"},"wrong_direction"],
    ]) {
      const invalid=connect(label);await invalid.connect();const socket=sockets.at(-1);
      const closed=new Promise(resolve=>socket.once("close",(code,detail)=>resolve({code,reason:String(detail)})));
      socket.send(JSON.stringify(frame));
      const result=await Promise.race([closed,delay(3000,undefined,{ref:false}).then(()=>assert.fail("malformed legacy frame not fenced"))]);
      assert.equal(result.code,1008);assert.equal(result.reason.split(":")[0],reason);trace.push({invalid:label,...result});await invalid.close();
    }
    evidence.observed={before:before.structuredResult.output,parallel:parallel.structuredResult.output,pending:completed.structuredResult.output,
      after:after.structuredResult.output,legacy_json_pongs:wire.filter(row=>row.frame?.type==="pong").length,
      hibernation:true,reconnected:true,disconnected_status:disconnected.structuredResult.status,disconnected_effect_count:1,
      disconnected_call_frames:lostFrames.length,fresh_after_disconnect:fresh.structuredResult.output,
      recovered_commands:0,matched_pongs:true,effect_count:1,invalid_frame_close:1008};
    console.log(JSON.stringify({evidence:output,...evidence.observed}));
  }catch(error){failure=error;evidence.error=error.stack;throw error;}
  finally {
    await writeFile(join(workspace,"release"),"cleanup");await writeFile(join(workspace,"release-disconnected"),"cleanup");
    await pending?.catch(()=>{});await disconnectedPending?.catch(()=>{});
    await Promise.all(connectors.map(value=>value.close()));for(const socket of sockets)socket.terminate();
    await tools.close();await native.close();await mf.dispose();
    await writeFile(join(output,"trace.json"),JSON.stringify({evidence,trace},null,2));
    await writeFile(join(output,"wire.json"),JSON.stringify(wire,null,2));await writeFile(join(output,"runtime.log"),runtime.join("\n"));
    await writeFile(join(output,"README.md"),"Command: "+evidence.command+"\nInputs: "+JSON.stringify(evidence.inputs)+"\nExpected: "+JSON.stringify(evidence.expected)+"\nObserved: "+JSON.stringify(evidence.observed)+"\nStatus: "+(failure?failure.stack:"PASS")+"\nEvidence: trace.json,wire.json,runtime.log,source-resolution.json,worker.mjs,hand/,sqlite/. Only admission credentials are synthetic; historical publisher, broker, Workerd WS, SQLite and native /bin/sh execute unchanged.\n");
  }
});
