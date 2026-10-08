import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { test } from 'node:test';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import WebSocket from 'ws';

const root = fileURLToPath(new URL('..', import.meta.url));
const source = `
import { DurableObject } from 'cloudflare:workers';
import { HostedToolsBroker } from './src/hosted-tools-broker.ts';
import { DiagnosticJournal } from './src/diagnostic-journal.ts';
export class Fixture extends DurableObject {
  constructor(ctx,env) {
    super(ctx,env);
    this.journal = new DiagnosticJournal(ctx.storage,'hand.broker');
    this.broker = new HostedToolsBroker(ctx,{resumeRetainedSockets:true,
      onCallObservation: row => this.journal.record({type:'hand.call.broker',...row})});
  }
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if(path==='/attach') return this.broker.upgrade('fixture-session');
    if(path==='/diagnostics') return Response.json(this.journal.page('fixture-thread',0,100));
    if(path==='/journal') {this.journal.record(await request.json());return Response.json({ok:true});}
    const body=await request.json();
    const tool=this.broker.provider().resolve('fixture__lookup');
    if(!tool) return new Response(null,{status:503});
    return Response.json(await tool.handler(body.input,{sessionId:'fixture-session',threadId:'fixture-thread',
      callId:body.call,model:body.model??'fixture',turnId:body.turn,signal:request.signal}));
  }
  webSocketMessage(socket,message) {return this.broker.webSocketMessage(socket,message);}
  webSocketClose(socket,code,reason) {this.broker.webSocketClose(socket,code,reason);}
  webSocketError(socket) {this.broker.webSocketError(socket);}
}
export default {fetch(request,env) {return env.BROKER.getByName('fixture').fetch(request);}};
`;

test('conflict diagnostics survive SQLite with only field names and keep the socket fence', {timeout:30000}, async () => {
  const output=join(root,'../../output/hand-conflict-diagnostics',String(Date.now()));
  await mkdir(output,{recursive:true});
  const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,
    format:'esm',platform:'node',target:'es2022',external:['cloudflare:*','node:*'],
    alias:{'nanocodex-tools/hosted':join(root,'../nanocodex-tools/src/hosted/index.ts')}});
  const mf=new Miniflare({port:0,modules:true,script:bundle.outputFiles[0].text,
    compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat','enable_request_signal'],
    durableObjects:{BROKER:{className:'Fixture',useSQLite:true}},durableObjectsPersist:join(output,'sqlite')});
  let socket;
  const trace=[];
  try {
    const base=await mf.ready;
    const api=async(path,body)=>{
      const response=await fetch(new URL(path,base),{method:body===undefined?'GET':'POST',
        headers:{'content-type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
      assert.equal(response.status,200);return response.json();
    };
    const url=new URL('/attach',base);url.protocol='ws:';
    socket=new WebSocket(url);
    await once(socket,'open');
    const ready=once(socket,'message');
    socket.send(JSON.stringify({type:'catalog',capabilities:['turn_metadata'],tools:[{
      provider:'fixture',remote_name:'lookup',definition:{type:'function',name:'fixture__lookup',description:'Fixture',
        strict:false,parameters:{type:'object',properties:{}}},parallel_safe:true,summary:'Fixture',timeout_ms:10000
    }]}));
    assert.equal(JSON.parse(String((await ready)[0])).type,'ready');
    let dispatches=0;
    let pendingReceived;
    const pendingArrived=new Promise(resolve=>{pendingReceived=resolve;});
    socket.on('message',data=>{
      const frame=JSON.parse(String(data));
      if(frame.type!=='call') return;
      dispatches++;
      if(frame.input.pending) {pendingReceived();return;}
      socket.send(JSON.stringify({type:'result',call_id:frame.call_id,outcome:{status:'completed',output:{
        output:'ok',success:true,structured_result:{output:'ok'},metadata:null,process_trace:null}}}));
    });
    const original={call:'retained-call',input:{value:'PRIVATE_ORIGINAL'},turn:'turn-original'};
    assert.equal((await api('/invoke',original)).success,true);
    assert.equal((await api('/invoke',original)).success,true);
    assert.equal(dispatches,1,'identical receipt replay must not redispatch');
    const benign=await api('/diagnostics');
    assert.ok(benign.events.filter(row=>row.stage==='replay').every(row=>!('conflict_fields' in row)));
    const pending=api('/invoke',{call:'pending-call',input:{pending:true}});
    await pendingArrived;
    const closed=once(socket,'close');
    const rejected=await api('/invoke',{...original,input:{value:'PRIVATE_CHANGED'},model:'PRIVATE_MODEL',turn:'turn-changed'});
    const collateral=await pending;
    assert.equal(rejected.structuredResult.status,'ambiguous');
    assert.match(collateral.structuredResult.message,/ambiguous after transport loss/);
    assert.equal((await closed)[0],1008);
    assert.equal(dispatches,2,'conflicting replay must not dispatch');
    const page=await api('/diagnostics');
    const conflict=page.events.find(row=>row.stage==='replay'&&row.reason_code==='call_conflict');
    assert.deepEqual(conflict.conflict_fields,['turn_id','model','input_json']);
    assert.equal(conflict.source_call_id,'retained-call');
    const serialized=JSON.stringify(page);
    for(const secret of ['PRIVATE_ORIGINAL','PRIVATE_CHANGED','PRIVATE_MODEL','turn-changed']) assert.ok(!serialized.includes(secret));
    // The journal boundary independently rejects arbitrary content even when
    // an observation producer supplies malformed field names.
    await api('/journal',{type:'fixture.projection',thread_id:'fixture-thread',conflict_fields:['input_json','PRIVATE_SENTINEL','input_json',{}]});
    const projected=(await api('/diagnostics')).events.find(row=>row.type==='fixture.projection');
    assert.deepEqual(projected.conflict_fields,['input_json']);
    trace.push({dispatches,conflict,rejected:rejected.structuredResult,collateral:collateral.structuredResult,projected});
    await writeFile(join(output,'evidence.json'),JSON.stringify(trace,null,2)+'\n');
    console.log(JSON.stringify({output,dispatches,fields:conflict.conflict_fields,close_code:1008,values_leaked:false}));
  } finally {socket?.terminate();await mf.dispose();}
});
