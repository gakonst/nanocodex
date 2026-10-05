// Real managed HTTP, account authorization, broker, SQLite and shipped WASM.
// Fixtures bootstrap synthetic accounts, answer external model HTTP, and acknowledge
// cleanup of a container that was never provisioned. A read-only route observes
// cleanup; all store/fork/import/export/delete behavior
// uses the shipped worker and public API, including actual SQLite and R2.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, writeFile, rm, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { builtinModules } from 'node:module';
import { createHash } from 'node:crypto';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const evidence = resolve(repo, process.env.NANOCODEX_CODE_STORE_EVIDENCE_DIR ?? 'output/managed-code-store-lifecycle');
const owner = '11111111-1111-4111-8111-111111111155';
const command = 'node --test js/managed/test/managed-code-store-lifecycle-journey.test.mjs';
const bootstrap = `
import managed, { DurableAgentSession } from './src/index.ts';
export * from './src/index.ts';
import { ensureAccount, createApiKey } from './src/account-auth.ts';
import { DurableObject } from 'cloudflare:workers';
// Cloudflare Containers is an external platform dependency. No container is
// provisioned in this journey; cleanup still crosses the actual DO RPC boundary.
export class EmptyContainerFixture extends DurableObject {
  async configure() {}
  async clearRemoteDesktop() { await this.ctx.storage.put('desktopCleared', true); }
  async destroy() { await this.ctx.storage.put('destroyed', true); }
  async fetch() { return Response.json(Object.fromEntries(await this.ctx.storage.list())); }
}

export class DocumentFixtureSession extends DurableAgentSession {
  async fetch(request) {
    if(new URL(request.url).pathname === '/__fixture/inspect') {
      // Observation only: no storage mutation, SDK replacement or execution.
      const counts={};
      for(const table of ['managed_code_cells','managed_code_store_versions','managed_code_store_blobs','managed_code_store_chunks']) counts[table]=this.ctx.storage.sql.exec('SELECT count(*) AS count FROM '+table).one().count;
      const blobs=this.ctx.storage.sql.exec('SELECT blob_key,bytes FROM managed_code_store_blobs').toArray();
      const exported=await this.ctx.storage.get('nanocodex:durability-exported');
      return Response.json({counts,blobs,exported:exported??null});
    }
    return super.fetch(request);
  }
}
export default {async fetch(request,env,ctx) {
  const path=new URL(request.url).pathname;
  if(path==='/__fixture') {
    const {user,capabilities}=await request.json(); await ensureAccount(env,user,true);
    const auth=await (await env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env,{kind:'api_key',userId:user,...auth.grant,...(capabilities?{capabilities}:{}),subjectId:'fixture:'+user,credentialId:'fixture'},'synthetic-historical-document-fork'));
  }
  if(path==='/__fixture/openai') return env.NANOCODEX.fetch('https://broker.internal/users/${owner}/credentials/openai',{method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({api_key:'sk-synthetic-document-fork'})});
  if(path.startsWith('/__fixture/sandbox/')) return env.NANOCODEX_SANDBOXES.getByName('nanocodex-'+path.split('/')[3]).fetch('https://sandbox.internal/inspect');
  const fixture=path.startsWith('/__fixture/inspect/') ? path.split('/') : null;
  if(fixture) return env.NANOCODEX_SESSIONS.getByName(fixture[3]).fetch(new Request('https://session.internal/__fixture/inspect',request));
  return managed.fetch(request,env,ctx);
}};
`;
async function bundle(source,cwd,name) {
  const wasm=new Set();
  // Reused workspace dependencies must resolve to this checkout's SDK once.
  const sdk=JSON.parse(await readFile(resolve(repo,'js/nanocodex/package.json'),'utf8'));
  const sdkAliases=Object.fromEntries(Object.entries(sdk.exports).map(([key,value])=>['nanocodex'+(key==='.'?'':key.slice(1)),resolve(repo,'js/nanocodex',value.import)]));
  const output=await build({stdin:{contents:source,resolveDir:cwd},bundle:true,write:false,format:'esm',platform:'browser',target:'es2022',external:['cloudflare:*','node:*'],
    alias:{...sdkAliases,'node-rsa':resolve(repo,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'real-wasm',setup(b){
      b.onResolve({filter:/^[a-z][a-z_\/]*$/},args=>builtinModules.includes(args.path)?{path:'node:'+args.path,external:true}:undefined);
      b.onResolve({filter:/\.wasm$|^nanocodex\/wasm$/},args=>{const path=args.path==='nanocodex/wasm'?resolve(repo,'js/nanocodex/pkg-web/nanocodex_bg.wasm'):resolve(args.resolveDir,args.path);wasm.add(path);return {path,external:true};});
    }}]});
  const code=output.outputFiles[0].text;
  const requires=[...new Set([...code.matchAll(/__require\("(node:[^"]+)"\)/g)].map(match=>match[1]))];
  const prelude=requires.map((name,index)=>`import * as builtin${index} from ${JSON.stringify(name)};`).join('\n')+`\nconst requireMap={${requires.map((name,index)=>`${JSON.stringify(name)}:builtin${index}`).join(',')}}; const require=name=>{if(!requireMap[name])throw new Error('Unexpected require '+name);return requireMap[name];};\n`;
  const path=resolve(evidence,name+'.mjs');await writeFile(path,prelude+code);
  return [{type:'ESModule',path},...Array.from(wasm,path=>({type:'CompiledWasm',path}))];
}
const externalProvider = `export default {async fetch(request,env) {
  if(request.headers.get('upgrade')?.toLowerCase() !== 'websocket') return env.CONTROL.fetch(request);
  const [client,server]=Object.values(new WebSocketPair()); server.accept();
  server.addEventListener('close',()=>server.close(1000,'fixture acknowledged'));
  server.addEventListener('message',async event=>{
    try {
      const {type,...body}=JSON.parse(event.data);
      if(type !== 'response.create') throw new Error('unexpected provider frame '+type);
      const headers=new Headers(request.headers); headers.delete('upgrade'); headers.set('content-type','application/json');
      const response=await env.CONTROL.fetch(new Request(request.url,{method:'POST',headers,body:JSON.stringify(body)}));
      if(!response.ok) throw new Error('provider rejected '+response.status);
      const stream=await response.text();
      for(const line of stream.split('\\n')) if(line.startsWith('data: ')) server.send(line.slice(6));
    }catch(error){server.send(JSON.stringify({type:'error',error:{code:'fixture_error',message:String(error)}}));}
  });
  return new Response(null,{status:101,webSocket:client});
}};`;
test('managed Code Mode rejects lossy portability before fencing, forks 5 MiB cold, and deletes its journal', {timeout:180_000}, async t=>{
  await mkdir(evidence,{recursive:true});
  const trace=[],upstream=[],failures=[],journalChecks=[];let mf,token,restarts=0,modelCalls=0;
  const cells={
    STORE_LARGE:'store("memo","x".repeat(5*1024*1024)); text("MEMO_STORED_5242880");',
    READ_LARGE:'const value=load("memo"); if(value?.length!==5*1024*1024 || value[0]!=="x" || value.at(-1)!=="x")throw new Error("large memo lost"); text("MEMO_READ_5242880");',
    OVERFLOW:'store("second","y".repeat(4*1024*1024));',
    CHILD_WRITE:'if(load("memo")?.length!==5*1024*1024)throw new Error("fork memo lost"); store("branch","child"); text("CHILD_WRITE_OK");',
    CHILD_READ:'if(load("memo")?.length!==5*1024*1024 || load("branch")!=="child" || load("second")!==undefined)throw new Error("cold fork store lost"); text("CHILD_COLD_OK");',
    PARENT_READ:'if(load("memo")?.length!==5*1024*1024 || load("branch")!==undefined || load("second")!==undefined)throw new Error("parent store mutated"); text("PARENT_OK");',
  };
  const provider=async request=>{
    try {
      const url=new URL(request.url);
      if(url.origin==='https://api.openai.com'&&url.pathname==='/v1/responses') {
        const body=await request.json();modelCalls++;
        if(body.model==='gpt-6-luna') return Response.json({id:'title-'+modelCalls,output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'Code store lifecycle'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}});
        const lastUser=body.input.findLastIndex(row=>row.role==='user');
        const prompt=JSON.stringify(body.input[lastUser]);
        const marker=Object.keys(cells).find(key=>prompt.includes('"'+key+'"'));
        let output=[{type:'message',role:'assistant',content:[{type:'output_text',text:'STORE_JOURNEY_OK'}]}];
        if(marker) {
          const callId='store-journey-'+marker+'-'+modelCalls;
          const result=body.input.slice(lastUser+1).find(row=>row.type==='custom_tool_call_output'&&row.call_id.startsWith('store-journey-'+marker+'-'));
          if(result) {
            const encoded=JSON.stringify(result);
            if(marker==='OVERFLOW') assert.match(encoded,/exceeds 8 MiB/);
            else {assert.doesNotMatch(encoded,/error_message|memo lost|store lost|store mutated/);assert.match(encoded,/_OK|_5242880/);}
            journalChecks.push({marker,result});
          }else output=[{type:'custom_tool_call',call_id:callId,name:'exec',input:cells[marker]}];
        }
        upstream.push({marker,model:body.model,input:body.input,output});
        return new Response(`data: ${JSON.stringify({type:'response.completed',response:{id:'response-'+modelCalls,status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}})}

`,{headers:{'content-type':'text/event-stream'}});
      }
      return new Response('unavailable synthetic external dependency',{status:502});
    }catch(error){failures.push(String(error));throw error;}
  };
  const managed=await bundle(bootstrap,resolve(repo,'js/managed'),'managed');
  const egress=await bundle(`export * from './src/egress.ts'; export {default} from './src/egress.ts';`,resolve(repo,'js/egress'),'egress');
  const persistence=resolve(evidence,'sqlite-'+crypto.randomUUID());
  const date='2026-07-29',flags=['nodejs_compat','enable_request_signal'];
  const options={durableObjectsPersist:persistence,r2Persist:resolve(persistence,'r2'),cachePersist:resolve(persistence,'cache'),workers:[
    {name:'managed',modulesRoot:'/',modules:managed,compatibilityDate:date,compatibilityFlags:flags,bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},
      serviceBindings:{NANOCODEX:'egress',NANOCODEX_SESSION_MODEL_EGRESS:{name:'egress',entrypoint:'SessionModelEgress'}},
      durableObjects:Object.fromEntries([['NANOCODEX_SANDBOXES','EmptyContainerFixture'],['NANOCODEX_AUTH','NonceStorage'],['NANOCODEX_USERS','UserAccount'],['NANOCODEX_ORGANIZATIONS','Organization'],['NANOCODEX_API_KEYS','ApiKeyRecord'],['NANOCODEX_SESSIONS','DocumentFixtureSession'],['NANOCODEX_ACCOUNT_TOOLS','AccountHostedTools'],['NANOCODEX_VM_HOST_POOLS','VmHostPool'],['NANOCODEX_MEMORY','MemoryScope']].map(([binding,className])=>[binding,{className,useSQLite:true}])),r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],outboundService:'external-provider'},
    {name:'egress',modulesRoot:'/',modules:egress,compatibilityDate:date,compatibilityFlags:flags,bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},serviceBindings:{MANAGED_AGENT_OWNERSHIP:{name:'managed',entrypoint:'ManagedAgentOwnership'}},
      durableObjects:Object.fromEntries([['USER_CREDENTIALS','UserCredentialBroker'],['AGENT_SUBJECTS','AgentSubjectDirectory'],['USER_CONNECTORS','UserConnectorBroker'],['MCP_CONNECTIONS','McpConnectionDirectory'],['SPOTIFY_RATE_LIMITS','SpotifyRateLimit'],['GMAIL_PUSH_MAILBOXES','GmailPushMailbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),outboundService:'external-provider'},
    {name:'external-provider',script:externalProvider,modules:true,compatibilityDate:date,compatibilityFlags:flags,serviceBindings:{CONTROL:provider}}]};
  const restart=async()=>{await mf?.dispose();mf=new Miniflare(options);restarts++;};
  const call=async(path,method='GET',body,status=200,headers={})=>{
    const response=await mf.dispatchFetch('https://nanocodex.example'+path,{method,headers:{authorization:'Bearer '+token,'content-type':'application/json',origin:'https://nanocodex.example',...headers},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const text=await response.text();let value;try{value=JSON.parse(text);}catch{value=text;}
    trace.push({path,method,status:response.status,value:path==='/__fixture'?{private_fields:'redacted'}:value});
    assert.ok((Array.isArray(status)?status:[status]).includes(response.status),`${method} ${path}: ${response.status} ${text}`);return value;
  };
  const turn=async(agent,input,id)=>{
    const receipt=await call(`/v1/agents/${agent}/turns`,'POST',{id,input},202);
    let result;for(let n=0;n<500;n++){result=await call(`/v1/agents/${agent}/turns/${receipt.turn_id??id}`);if(['completed','failed','cancelled'].includes(result.state))break;await new Promise(r=>setTimeout(r,10));}
    assert.equal(result.state,'completed',JSON.stringify(result));assert.match(JSON.stringify(result),/STORE_JOURNEY_OK/);return result;
  };
  const inspect=agent=>call('/__fixture/inspect/'+agent);
  let outcomes;
  try {
    await restart();token=(await call('/__fixture','POST',{user:owner})).token;
    await call('/__fixture/openai','POST',undefined,204);
    const settings={model:'gpt-6.1-sol',thinking:'low',reasoning_mode:'standard',fast_mode:false};
    const parent=(await call('/v1/agents','POST',{settings},201)).agent_id;
    await turn(parent,'STORE_LARGE','large');await restart();
    const initial=await inspect(parent);
    assert.ok(initial.blobs.some(blob=>blob.blob_key.startsWith('session:')&&blob.bytes>5*1024*1024));
    assert.ok(Object.values(initial.counts).every(count=>count>0));
    assert.equal((await call(`/v1/agents/${parent}/durability`,'POST',undefined,409)).error,'code_mode_store_not_portable');
    assert.equal((await inspect(parent)).exported,null,'refusal must not persist the source seal');
    await turn(parent,'READ_LARGE','read-after-refusal');await restart();
    await turn(parent,'READ_LARGE','cold-read-after-refusal');
    await turn(parent,'OVERFLOW','overflow');await restart();
    await turn(parent,'PARENT_READ','after-overflow');await restart();
    const child=(await call(`/v1/agents/${parent}/forks`,'POST',{at:'large'},201,{'idempotency-key':'large-fork'})).agent_id;
    await restart();await turn(child,'CHILD_WRITE','child-write');await restart();
    await turn(child,'CHILD_READ','child-cold');await restart();
    await turn(parent,'PARENT_READ','parent-cold');await restart();
    // A genuine shipped archive from a no-Code-Mode source exercises old
    // pointer-only archive rejection without replacing importer behavior.
    const portable=(await call('/v1/agents','POST',{settings},201)).agent_id;
    await turn(portable,'NO_STORE','portable');await restart();
    let archive;
    for(let n=0;n<40;n++){
      archive=await call(`/v1/agents/${portable}/durability`,'POST',undefined,[200,202]);
      if(archive.stage!=='exporting')break;
      await new Promise(resolve=>setTimeout(resolve,1000));
    }
    assert.ok(archive.durability,'portable export must finish');
    const legacy=structuredClone(archive);
    const payload=JSON.parse(legacy.durability.payload);
    assert.ok(payload.nanocodex_durable_state.documents.current);
    payload.nanocodex_durable_state.documents.current['nanocodex.managed.code-store']={version:1,value:{format:1,hash:'a'.repeat(64),bytes:5*1024*1024},fork:'asOf'};
    legacy.durability.payload=JSON.stringify(payload);
    const beforeImport=await call('/v1/agents');
    for(const durability of [legacy,legacy.durability]) {
      assert.equal((await call('/v1/agents','POST',{durability},409,{'idempotency-key':'legacy-refusal'})).error,'code_mode_store_not_portable');
    }
    assert.deepEqual(await call('/v1/agents'),beforeImport,'rejected import must not create any destination');
    const pristine=(await call('/v1/agents','POST',{durability:archive},201,{'idempotency-key':'legacy-refusal'})).agent_id;
    await restart();await turn(pristine,'NO_STORE','imported');
    const beforeDelete=await inspect(child);assert.ok(Object.values(beforeDelete.counts).every(count=>count>0));
    await call(`/v1/agents/${child}`,'DELETE',undefined,204);await restart();
    assert.deepEqual(await call('/__fixture/sandbox/'+child),{desktopCleared:true,destroyed:true});
    const afterDelete=await inspect(child);assert.ok(Object.values(afterDelete.counts).every(count=>count===0));
    await call(`/v1/agents/${child}`,'GET',undefined,404);
    await turn(parent,'PARENT_READ','surviving-parent');
    assert.deepEqual(failures,[]);
    assert.deepEqual(journalChecks.map(check=>check.marker),['STORE_LARGE','READ_LARGE','READ_LARGE','OVERFLOW','PARENT_READ','CHILD_WRITE','CHILD_READ','PARENT_READ','PARENT_READ']);
    outcomes={parent,child,pristine,memoBytes:5*1024*1024,preFenceExportRefusal:true,continuedAfterRefusal:true,legacyImportPristineRefusal:true,overflowRolledBack:true,coldForkIsolated:true,beforeDelete,afterDelete};
    t.diagnostic(JSON.stringify({outcomes,restarts,modelCalls,journalChecks}));
  }finally {
    await mf?.dispose();
    await writeFile(resolve(evidence,'public-api-trace.json'),JSON.stringify(trace,null,2));
    await writeFile(resolve(evidence,'provider-trace.json'),JSON.stringify(upstream,null,2));
    await writeFile(resolve(evidence,'result.json'),JSON.stringify({command,outcomes,restarts,modelCalls,journalChecks,failures,wasmSha256:createHash('sha256').update(await readFile(resolve(repo,'js/nanocodex/pkg-web/nanocodex_bg.wasm'))).digest('hex')},null,2));
    await rm(persistence,{recursive:true,force:true});
  }
});
