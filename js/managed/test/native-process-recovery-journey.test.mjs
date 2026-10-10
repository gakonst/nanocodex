import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import WebSocket from 'ws';
import { createTools } from 'nanocodex/tools';
import { createAttachment } from 'nanocodex-tools/attachment';
import { createNodeProcessTools } from 'nanocodex-tools/node';
import { fetch } from "./support/miniflare-fetch.mjs";

const root=fileURLToPath(new URL('..',import.meta.url));
const repo=fileURLToPath(new URL('../../../',import.meta.url));
const owner='00000000-0000-4000-8000-000000000071';
const machine='recovery-hand';
// Reconstruct the production namespace and account proxy over real SQLite,
// leaving the actual broker, WebSocket publisher and PTY alive. No model needed.
const source=`
import { DurableObject } from 'cloudflare:workers';
import { AccountHostedTools, AccountHostedToolsProvider } from './src/account-hosted-tools.ts';
import { NamespaceProcessSessions } from './src/namespace-process-storage.ts';
import { createNamespaceExecutionRuntime } from './src/namespace-tools.ts';
export { AccountHostedTools };
export class Harness extends DurableObject {
  provider; runtime; authority='account'; snapshots=0;
  reconstruct() {
    new NamespaceProcessSessions(this.ctx.storage).get(1);
    this.provider=new AccountHostedToolsProvider(this.env.HANDS,'${owner}',()=>this.authority==='account');
    this.runtime=createNamespaceExecutionRuntime(
      ()=>this.provider.machines().map(m=>({id:'user:'+m.id,root:'/hand',workspace:m.workspace})),
      (id,name,ctx)=>this.provider.machineTool(id.slice(5),name,ctx),undefined,undefined,
      ()=>this.authority,new NamespaceProcessSessions(this.ctx.storage),'fixture-thread',
      (binding,ctx)=>this.provider.recoverProcessTool(binding.machineId.slice(5),binding.processSessionKey,ctx));
  }
  async fetch(request) {
    const body=await request.json();
    if(!this.runtime||body.cold) this.reconstruct();
    this.authority=body.authority??'account';
    if(body.tool==='exec_command') { await this.provider.refreshMachine('${machine}',{sessionId:'fixture-session'});this.snapshots++; }
    const context={sessionId:body.session??'fixture-session',callId:body.call, parentCallId:body.call,model:'fixture',signal:request.signal};
    try { const result=await this.runtime.tools[body.tool].handler(body.input,context);
      return Response.json({result:result.structuredResult??result,snapshots:this.snapshots,
        bindings:this.ctx.storage.sql.exec('SELECT COUNT(*) AS n FROM managed_namespace_processes').one().n});
    } catch(error) {return Response.json({error:error.message,snapshots:this.snapshots});}
  }
}
export default {fetch(request,env) {
  if(new URL(request.url).pathname==='/tool-host') return env.HANDS.getByName('${owner}').fetch(request);
  return env.SESSION.getByName('fixture').fetch(request);
}};
`;
test('native PTY survives namespace/provider reconstruction with exact persisted route', {timeout:60000},async()=>{
  const output=join(repo,'output/native-process-recovery-journey',String(Date.now()));
  await mkdir(output,{recursive:true});
  const transcript=[], wire=[];let mf,native,tools,attachment;
  try {
    const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,format:'esm',platform:'node',conditions:['workerd'],target:'es2022',external:['cloudflare:*','node:*'],banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},alias:{'node-rsa':join(root,'../nanocodex/tools/browser/unsupportedNodeRsa.mjs'),'nanocodex-tools/hosted':join(repo,'js/nanocodex-tools/src/hosted/index.ts')},logLevel:'silent'});
    mf=new Miniflare({port:0,compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat','enable_request_signal'],modules:[{type:'ESModule',path:'worker.mjs',contents:bundle.outputFiles[0].text}],durableObjects:{HANDS:{className:'AccountHostedTools',useSQLite:true},SESSION:{className:'Harness',useSQLite:true}},durableObjectsPersist:join(output,'sqlite')});
    const base=await mf.ready;
    native=await createNodeProcessTools({workspace:output});tools=await createTools({tools:native.tools});
    const connect=async()=>{
      const endpoint=new URL('/tool-host',base);endpoint.protocol='ws:';
      attachment=createAttachment(tools,{endpoint:endpoint.href,transport:{connect(){
        const socket=new WebSocket(endpoint,{headers:{'x-nanocodex-owner-id':owner}});
        socket.on('message',data=>{const f=JSON.parse(String(data));wire.push({type:f.type,name:f.name??f.tool_name});});
        return socket;
      }}},{machines:[{id:machine,name:'Recovery fixture',workspace:output,capabilities:['shell']}],attachmentId:machine});
      assert.equal((await attachment.connect()).connected,true);
    };
    await connect();let call=0;
    const invoke=async(body)=>{const response=await fetch(base,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...body,call:'call-'+(++call)})});const value=await response.json();transcript.push({input:body,...value});return value;};
    // util-linux script allocates the PTY; test -t verifies it inside the child.
    const started=await invoke({tool:'exec_command',input:{cmd:"script -qec 'test -t 0 || exit 99; printf ONCE >> effect.log; printf READY; read first; printf GOT:%s \"$first\"; read second; printf DONE' /dev/null",workdir:'/hand',shell:'/bin/sh',login:false,yield_time_ms:100}});
    assert.ok(started.result,JSON.stringify(started)); const id=started.result.session_id;assert.ok(id,JSON.stringify(started));assert.equal(started.bindings,1);
    const rejectedStart=wire.length;
    const wrong=await invoke({cold:true,authority:'revoked',tool:'write_stdin',input:{session_id:id,chars:'NO\n',yield_time_ms:1}});
    assert.match(wrong.error,/unknown or stale/);
    const sibling=await invoke({cold:true,session:'sibling',tool:'write_stdin',input:{session_id:id,chars:'NO\n',yield_time_ms:1}});
    assert.match(sibling.error,/unknown or stale/);
    assert.equal(wire.slice(rejectedStart).filter(f=>f.type==='call').length,0);
    const resumed=await invoke({cold:true,tool:'write_stdin',input:{session_id:id,chars:'recovered\n',yield_time_ms:100}});
    assert.equal(resumed.result.session_id,id);assert.match(resumed.result.output,/GOT:recovered/);assert.equal(resumed.snapshots,1);
    const completed=await invoke({cold:true,tool:'write_stdin',input:{session_id:id,chars:'finish\n',yield_time_ms:1000}});
    assert.match(completed.result.output,/DONE/);assert.equal(completed.result.exit_code,0);assert.equal(completed.bindings,0);
    const second=await invoke({tool:'exec_command',input:{...transcript[0].input.input}});
    const secondId=second.result.session_id;assert.ok(secondId);assert.notEqual(secondId,id);assert.equal(second.bindings,1);
    // Replacement publisher with the same machine but a new runtime must never
    // receive the saved stdin. Native process remains alive until test cleanup.
    await attachment.close();await connect();
    const before=wire.length;
    const replaced=await invoke({cold:true,tool:'write_stdin',input:{session_id:secondId,chars:'MUST_NOT_REACH\n',yield_time_ms:100}});
    assert.match(JSON.stringify(replaced),/runtime changed|continuity/);
    assert.equal(replaced.bindings,1);assert.equal(replaced.snapshots,2);
    assert.equal(wire.slice(before).filter(f=>f.type==='call').length,0);
    assert.equal(await readFile(join(output,'effect.log'),'utf8'),'ONCEONCE');
    await writeFile(join(output,'result.json'),JSON.stringify({passed:true,transcript,wire},null,2));
    console.log('Recovery evidence:',output);
  } finally {
    await writeFile(join(output,'transcript.json'),JSON.stringify(transcript,null,2));
    await attachment?.close();await tools?.close();await native?.close();await mf?.dispose();
  }
});
