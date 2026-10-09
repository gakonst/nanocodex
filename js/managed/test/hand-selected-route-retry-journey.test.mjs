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

const root=fileURLToPath(new URL('..',import.meta.url));
const repo=fileURLToPath(new URL('../../../',import.meta.url));
const owner='00000000-0000-4000-8000-000000000072';
const machine='retry-hand';
// Production account broker, provider and namespace runtime over real SQLite
// with a real WebSocket publisher. The selected-machine lookup is exercised
// exactly as a workdir-routed exec_command performs it.
const source=`
import { DurableObject } from 'cloudflare:workers';
import { AccountHostedTools, AccountHostedToolsProvider } from './src/account-hosted-tools.ts';
import { NamespaceProcessSessions } from './src/namespace-process-storage.ts';
import { createNamespaceExecutionRuntime } from './src/namespace-tools.ts';
export { AccountHostedTools };
export class Harness extends DurableObject {
  async fetch(request) {
    const body=await request.json();
    // A fresh provider has no reusable lookup: every request performs one.
    const provider=new AccountHostedToolsProvider(this.env.HANDS,'${owner}',()=>true);
    const runtime=createNamespaceExecutionRuntime(
      ()=>provider.machines().map(m=>({id:'user:'+m.id,root:'/hand',workspace:m.workspace})),
      (id,name,ctx)=>provider.machineTool(id.slice(5),name,ctx),undefined,undefined,
      ()=>'account',new NamespaceProcessSessions(this.ctx.storage),'fixture-thread',
      (binding,ctx)=>provider.recoverProcessTool(binding.machineId.slice(5),binding.processSessionKey,ctx));
    const started=Date.now();
    const context={sessionId:'fixture-session',callId:body.call,parentCallId:body.call,model:'fixture',signal:request.signal};
    try {
      await provider.refreshMachine('${machine}',{sessionId:'fixture-session'},false,false,request.signal);
      const result=await runtime.tools.exec_command.handler(body.input,context);
      return Response.json({result:result.structuredResult??result,elapsed_ms:Date.now()-started});
    } catch(error) {return Response.json({error:error.message,elapsed_ms:Date.now()-started});}
  }
}
export default {fetch(request,env) {
  if(new URL(request.url).pathname==='/tool-host') return env.HANDS.getByName('${owner}').fetch(request);
  return env.SESSION.getByName('fixture').fetch(request);
}};
`;
test('a selected Hand that reconnects during lookup is retried once; a missing one fails finitely', {timeout:60000},async()=>{
  const output=join(repo,'output/hand-selected-route-retry-journey',String(Date.now()));
  await mkdir(output,{recursive:true});
  const transcript=[];let mf,native,tools,attachment;
  try {
    const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,format:'esm',platform:'node',conditions:['workerd'],target:'es2022',external:['cloudflare:*','node:*'],banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},alias:{'node-rsa':join(root,'../nanocodex/tools/browser/unsupportedNodeRsa.mjs'),'nanocodex-tools/hosted':join(repo,'js/nanocodex-tools/src/hosted/index.ts')},logLevel:'silent'});
    mf=new Miniflare({port:0,compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat','enable_request_signal'],modules:[{type:'ESModule',path:'worker.mjs',contents:bundle.outputFiles[0].text}],durableObjects:{HANDS:{className:'AccountHostedTools',useSQLite:true},SESSION:{className:'Harness',useSQLite:true}},durableObjectsPersist:join(output,'sqlite')});
    const base=await mf.ready;
    native=await createNodeProcessTools({workspace:output});tools=await createTools({tools:native.tools});
    const connect=async()=>{
      const endpoint=new URL('/tool-host',base);endpoint.protocol='ws:';
      attachment=createAttachment(tools,{endpoint:endpoint.href,transport:{connect(){
        return new WebSocket(endpoint,{headers:{'x-nanocodex-owner-id':owner}});
      }}},{machines:[{id:machine,name:'Retry fixture',workspace:output,capabilities:['shell']}],attachmentId:machine});
      assert.equal((await attachment.connect()).connected,true);
    };
    let call=0;
    const invoke=async(input)=>{const response=await fetch(base,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({input,call:'call-'+(++call)})});const value=await response.json();transcript.push({input,...value});return value;};
    const command=(text)=>({cmd:"printf '%s' "+text+" >> effect.log; printf "+text,workdir:'/hand',shell:'/bin/sh',login:false,yield_time_ms:2000});

    await connect();
    const first=await invoke(command('FIRST'));
    assert.equal(first.result?.output,'FIRST',JSON.stringify(first));

    // The Hand socket is replaced: the route is unpublished when the call
    // arrives and republished shortly afterwards. One fresh retry admits it.
    await attachment.close();
    const reconnect=new Promise(resolve=>setTimeout(resolve,300)).then(connect);
    const retried=await invoke(command('RETRIED'));
    await reconnect;
    assert.equal(retried.result?.output,'RETRIED',JSON.stringify(retried));
    assert.ok(retried.elapsed_ms>=1000,'the call waited for the bounded retry: '+retried.elapsed_ms);

    // A Hand that stays offline fails after exactly one retry, not indefinitely.
    await attachment.close();attachment=undefined;
    const missing=await invoke(command('MISSING'));
    assert.match(missing.error??'',/Selected Hand route unavailable/,JSON.stringify(missing));
    assert.ok(missing.elapsed_ms>=1000&&missing.elapsed_ms<15000,'bounded failure: '+missing.elapsed_ms);
    assert.equal(await readFile(join(output,'effect.log'),'utf8'),'FIRSTRETRIED');
    await writeFile(join(output,'result.json'),JSON.stringify({passed:true,transcript},null,2));
    console.log('Selected route retry evidence:',output);
  } finally {
    await writeFile(join(output,'transcript.json'),JSON.stringify(transcript,null,2));
    await attachment?.close();await tools?.close();await native?.close();await mf?.dispose();
  }
});
