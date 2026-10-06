#!/usr/bin/env node
// Real normal Managed API + account auth + Session SQLite/WASM + normal Egress.
// External OAuth/catalog/model HTTP and initial account identity are fixtures.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { builtinModules } from 'node:module';
import { dirname, resolve, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import { accountProxyWorker } from './account-proxy.mjs';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';
const here = dirname(fileURLToPath(import.meta.url));
const opts = Object.fromEntries(process.argv.slice(2).map(arg => { assert.match(arg,/^--[^=]+=/); const i=arg.indexOf('=');return [arg.slice(2,i),arg.slice(i+1)]; }));
const root=resolve(opts.root??join(here,'../../..')), label=opts.label??`run-${Date.now()}`, mode=opts.mode??'combined';
assert.match(label,/^[a-zA-Z0-9_-]+$/); assert.ok(['legacy','combined','stream'].includes(mode));
const count=Number(opts.samples??5), processCount=Number(opts['process-samples']??2);
assert.ok(Number.isSafeInteger(count)&&count>0&&count<=100);assert.ok(Number.isSafeInteger(processCount)&&processCount>=0&&processCount<=30);
const output=resolve(opts.output??join(root,'output/managed-api-ttft',label));
const owner='11111111-1111-4111-8111-111111111133', other='11111111-1111-4111-8111-111111111144', text='BENCHMARK_ASSISTANT_TEXT';
const credential=opts.credential??'chatgpt';assert.ok(['chatgpt','openai'].includes(credential));
const family=opts.family??'codex';assert.ok(['codex','claude'].includes(family));
const settings={model:family==='codex'?'gpt-6.1-sol':'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false};
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const rows=[],runtime=[],providerCalls=[],buildInfo=[];
const bootstrap=`import managed from './src/index.ts';export * from './src/index.ts';import {ensureAccount,createApiKey} from './src/account-auth.ts';export default {async fetch(request,env,ctx){if(new URL(request.url).pathname==='/__fixture/chatgpt'){const expires_at=(Math.ceil(Date.now()/1000)+3600)*1000;const payload={exp:Math.ceil(expires_at/1000),'https://api.openai.com/auth':{chatgpt_account_id:'synthetic-account',chatgpt_account_is_fedramp:false}};const jwt=btoa(JSON.stringify({alg:'none'})).replaceAll('=','')+'.'+btoa(JSON.stringify(payload)).replaceAll('=','')+'.fixture';return env.NANOCODEX.fetch('https://broker.internal/users/${owner}/credentials/chatgpt',{method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({access_token:jwt,refresh_token:'synthetic-refresh',account_id:'synthetic-account',expires_at,fedramp:false})});}if(new URL(request.url).pathname==='/__fixture/openai')return env.NANOCODEX.fetch('https://broker.internal/users/${owner}/credentials/openai',{method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({api_key:'sk-synthetic-openai-runtime'})});if(new URL(request.url).pathname==='/__fixture'){const {user,capabilities}=await request.json();await ensureAccount(env,user,true);const auth=await(await env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/authorization')).json();return Response.json(await createApiKey(env,{kind:'api_key',userId:user,...auth.grant,...(capabilities?{capabilities}:{}),subjectId:'fixture:'+user,credentialId:'fixture'},'synthetic-ttft'));}return managed.fetch(request,env,ctx);}};`;
await mkdir(output,{recursive:true});
const git=args=>{try{return execFileSync('git',['-C',root,...args],{encoding:'utf8'});}catch{return null;}};
const config={label,root,mode,family,credential,samples:count,process_samples:processCount,command:[process.execPath,...process.argv.slice(1)],git_head:git(['rev-parse','HEAD'])?.trim(),node:process.version,curl:execFileSync('curl',['--version'],{encoding:'utf8'}).split('\n')[0],started_at:new Date().toISOString(),methodology:'Actual js/account routeManaged front proxy, normal js/managed and js/egress production workers, real account/API-key authorization, account and Session SQLite DOs, Rust WASM runtime and SessionModelEgress. Only synthetic account bootstrap and external OAuth/catalog/model HTTP are fixtures. No live inference, Internet/TLS, Cloudflare geography, production latency claim. Local workerd on loopback. Stopwatch begins before first curl spawn and ends on first nonempty current-turn assistant.delta.text. Headers and initial SSE receipt are not TTFT. Legacy fresh=POST create + POST turn + GET SSE; combined fresh=POST agent-runs JSON + GET SSE; stream fresh=one POST agent-runs SSE. Warm existing sessions use one POST turn SSE in stream mode and POST turn + GET SSE otherwise. Process samples restart workerd; account/key/OAuth setup excluded and warms API/account/Egress before first Session. Fresh-session samples reuse process after recorded untimed warmup; warm samples are second turns of paired fresh sessions. No synthetic latency added during timing. Contract-only disconnect case holds provider text 100ms to verify receipt precedes completion.'};
await writeFile(join(output,'config.json'),JSON.stringify(config,null,2));
await writeFile(join(output,'harness.mjs'),await readFile(fileURLToPath(import.meta.url)));
await writeFile(join(output,'source.patch'),git(['diff','--','js/managed/src','js/egress/src','js/nanocodex'])??'unavailable');
await writeFile(join(output,'git-status.txt'),git(['status','--short'])??'unavailable');
async function bundle(name,source,cwd){
 const assets=[],seen=new Set(),lazy=new Set();
 const result=await build({stdin:{contents:source,resolveDir:cwd},bundle:true,write:false,metafile:true,format:'esm',platform:'browser',target:'es2022',external:['cloudflare:*','node:*'],alias:{'node-rsa':join(root,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'actual-wasm',setup(b){b.onResolve({filter:/\.\/just-bash-lazy\.mjs$/},a=>{const path=resolve(a.resolveDir,a.path);lazy.add(path);return {path,external:true};});b.onResolve({filter:/^[a-z][a-z_]*(?:\/[a-z_]+)?$/},a=>builtinModules.includes(a.path)?{path:'node:'+a.path,external:true}:undefined);b.onResolve({filter:/\.wasm(?:\?module)?$|^nanocodex\/wasm$/},a=>{const path=a.path==='nanocodex/wasm'?join(root,'js/nanocodex/pkg-web/nanocodex_bg.wasm'):resolve(a.resolveDir,a.path.replace(/\?module$/,''));seen.add(path);return {path,external:true};});}}],logLevel:'silent'});
 const code=result.outputFiles[0].text, requires=[...new Set([...code.matchAll(/__require\("(node:[^"]+)"\)/g)].map(m=>m[1]))];
 const prelude=requires.map((n,i)=>`import * as builtin${i} from ${JSON.stringify(n)};`).join('\n')+`\nconst requireMap={${requires.map((n,i)=>`${JSON.stringify(n)}:builtin${i}`).join(',')}};const require=name=>{if(!requireMap[name])throw new Error('Unexpected require '+name);return requireMap[name];};\n`;
 const path=join(output,name+'.mjs');await writeFile(path,prelude+code);
 const inputs={};for(const file of Object.keys(result.metafile.inputs)){if(file==='<stdin>'||file.startsWith('(disabled):'))continue;inputs[relative(root,resolve(file))]=hash(await readFile(resolve(file)));}
 for(const path of lazy){assets.push({type:'ESModule',path});inputs[relative(root,path)]=hash(await readFile(path));}
 for(const wasm of seen) assets.push({type:'CompiledWasm',path:wasm});
 buildInfo.push({name,sha256:hash(prelude+code),inputs,wasm:await Promise.all([...seen].map(async path=>({path:relative(root,path),sha256:hash(await readFile(path))})))});
 return [{type:'ESModule',path},...assets];
}
console.log('Building normal Managed API '+label);
const managed=await bundle('managed',bootstrap,join(root,'js/managed'));
const egress=await bundle('egress',`export * from './src/egress.ts';export {default} from './src/egress.ts';`,join(root,'js/egress'));
await writeFile(join(output,'source-hashes.json'),JSON.stringify(buildInfo,null,2));
const providerSource=`export default {async fetch(request,env){
 const url=new URL(request.url);
 if(!['https://api.openai.com','https://chatgpt.com'].includes(url.origin))return env.CONTROL.fetch(request);
 if(url.origin==='https://api.openai.com'&&request.headers.get('authorization')!=='Bearer sk-synthetic-openai-runtime')throw new Error('fixture bearer mismatch');
 if(url.origin==='https://chatgpt.com'&&request.headers.get('chatgpt-account-id')!=='synthetic-account')throw new Error('fixture ChatGPT account mismatch');
 for(const name of ['x-nanocodex-subject','x-nanocodex-session-model-owner','cookie'])if(request.headers.has(name))throw new Error('private header leaked '+name);
 if(request.headers.get('upgrade')!=='websocket'){
  const body=await request.json();if(body.model==='gpt-6-luna')return Response.json({id:'title',output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'Synthetic benchmark'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}});
  throw new Error('Expected production WebSocket transport');
 }
 const [client,server]=Object.values(new WebSocketPair());server.accept();server.addEventListener('close',()=>server.close(1000));
 server.addEventListener('message',async event=>{const body=JSON.parse(event.data);
  if(body.type!=='response.create')throw new Error('Unexpected provider frame '+body.type);
  if(body.model!=='gpt-6.1-sol'||!JSON.stringify(body.input).includes('BENCH_SAMPLE_'))throw new Error('Non synthetic benchmark prompt');
  await env.CONTROL.fetch('https://fixture.internal/__provider/trace',{method:'POST',body:JSON.stringify({model:body.model,input:body.input})});
  if(JSON.stringify(body.input).includes('BENCH_SAMPLE_disconnect'))await new Promise(r=>setTimeout(r,100));
  const id='resp_'+crypto.randomUUID();server.send(JSON.stringify({type:'response.created',response:{id,status:'in_progress'}}));
  server.send(JSON.stringify({type:'response.output_text.delta',output_index:0,delta:'BENCHMARK_ASSISTANT_TEXT'}));
  server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:true,output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'BENCHMARK_ASSISTANT_TEXT'}]}],usage:{input_tokens:10,output_tokens:2,total_tokens:12}}}));
 });return new Response(null,{status:101,webSocket:client});
}};`;
await writeFile(join(output,'provider.mjs'),providerSource);
const account=await accountProxyWorker(root);await writeFile(join(output,'account.mjs'),account.script);await writeFile(join(output,'account-source-hash.json'),JSON.stringify({bundle_sha256:hash(account.script),source_sha256:hash(await readFile(join(root,'js/account/worker/managedProxy.ts')))},null,2));
let processId=0;
async function boot(){
 const process=++processId;
 const provider=async request=>{
  const url=new URL(request.url);
  if(url.pathname==='/__provider/trace'){const body=await request.json();providerCalls.push({process,at_ms:Date.now(),model:body.model,request_sha256:hash(JSON.stringify(body))});return new Response(null,{status:204});}
  if(url.origin==='https://api.anthropic.com'&&url.pathname==='/v1/models')return Response.json({data:[{id:settings.model,display_name:'Synthetic benchmark Sonnet'}],has_more:false});
  if(url.origin==='https://api.anthropic.com'&&url.pathname==='/v1/messages'){
   assert.equal(request.headers.get('authorization'),'Bearer synthetic-claude-ttft-runtime');
   for(const name of ['x-nanocodex-subject','x-nanocodex-session-model-owner','x-api-key','cookie'])assert.equal(request.headers.has(name),false,'private header stripped: '+name);
   const body=await request.json();assert.equal(body.model,settings.model);assert.equal(body.stream,true);assert.match(JSON.stringify(body.messages),/BENCH_SAMPLE_/);
   providerCalls.push({process,at_ms:Date.now(),model:body.model,request_sha256:hash(JSON.stringify(body)),message_count:body.messages.length});if(JSON.stringify(body.messages).includes('BENCH_SAMPLE_disconnect'))await delay(100);
   const id='msg_'+randomUUID(),events=[{type:'message_start',message:{id,role:'assistant',model:settings.model,content:[],usage:{input_tokens:10,output_tokens:0}}},{type:'content_block_start',index:0,content_block:{type:'text',text:''}},{type:'content_block_delta',index:0,delta:{type:'text_delta',text}},{type:'content_block_stop',index:0},{type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage:{output_tokens:2}},{type:'message_stop'}];
   return new Response(events.map(e=>`event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
  }
  const response=await claudeProvider(request);if(response)return response;
  throw new Error('Unexpected external request '+url.origin+url.pathname);
 };
 const mf=new Miniflare({port:0,host:'127.0.0.1',handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])createInterface({input:stream}).on('line',line=>{runtime.push({process,line});});},workers:[account,
 {name:'managed',modulesRoot:'/',modules:managed,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},serviceBindings:{NANOCODEX:'egress',NANOCODEX_SESSION_MODEL_EGRESS:{name:'egress',entrypoint:'SessionModelEgress'}},durableObjects:Object.fromEntries([['NANOCODEX_AUTH','NonceStorage'],['NANOCODEX_USERS','UserAccount'],['NANOCODEX_ORGANIZATIONS','Organization'],['NANOCODEX_API_KEYS','ApiKeyRecord'],['NANOCODEX_SESSIONS','DurableAgentSession'],['NANOCODEX_ACCOUNT_TOOLS','AccountHostedTools'],['NANOCODEX_VM_HOST_POOLS','VmHostPool'],['NANOCODEX_MEMORY','MemoryScope']].map(([binding,className])=>[binding,{className,useSQLite:true}])),r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],outboundService:'provider'},
 {name:'egress',modulesRoot:'/',modules:egress,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},serviceBindings:{MANAGED_AGENT_OWNERSHIP:{name:'managed',entrypoint:'ManagedAgentOwnership'}},durableObjects:Object.fromEntries([['USER_CREDENTIALS','UserCredentialBroker'],['AGENT_SUBJECTS','AgentSubjectDirectory'],['USER_CONNECTORS','UserConnectorBroker'],['MCP_CONNECTIONS','McpConnectionDirectory'],['SPOTIFY_RATE_LIMITS','SpotifyRateLimit'],['GMAIL_PUSH_MAILBOXES','GmailPushMailbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),outboundService:'provider'},
 {name:'provider',compatibilityDate:'2026-07-29',modules:true,script:providerSource,serviceBindings:{CONTROL:provider}}]});
 const server={mf,base:String(await mf.ready),process};
 try{server.token=(await request(server,'/__fixture','POST',{user:owner})).value.token;if(family==='codex'){const seeded=await request(server,'/__fixture/'+credential,'POST');assert.equal(seeded.status,204,JSON.stringify(seeded));return server;}const login=(await request(server,'/v1/credentials/claude/login','POST')).value;const state=new URL(login.authorization_url).searchParams.get('state');await request(server,'/v1/credentials/claude/login/complete','POST',{code:'ttft-runtime#'+state});return server;}catch(error){await mf.dispose();throw error;}
}
async function request(server,path,method='GET',body,extra={}){const response=await fetch(new URL(path,server.base),{method,headers:{authorization:'Bearer '+server.token,'content-type':'application/json',...extra},...(body===undefined?{}:{body:JSON.stringify(body)}),signal:AbortSignal.timeout(30000)});const raw=await response.text();let value;try{value=JSON.parse(raw);}catch{value=raw;}return {status:response.status,value};}
function curl(server,path,method,body,extra,onFrame){
 const args=['-N','--silent','--show-error','--max-time','40','--include','-H','Authorization: Bearer '+server.token,'-H','Content-Type: application/json',...Object.entries(extra).flatMap(([k,v])=>['-H',k+': '+v]),'-X',method,...(body===undefined?[]:['--data-binary',JSON.stringify(body)]),new URL(path,server.base).href];
 const started=performance.now(), child=spawn('curl',args,{stdio:['ignore','pipe','pipe']});let stdout='',stderr='',head='',buffer='',headersDone=false,firstHeaders=null,firstBody=null,status;
 const done=new Promise((resolvePromise,reject)=>{child.on('error',reject);child.stdout.on('data',bytes=>{const at=performance.now()-started;let chunk=bytes.toString('utf8');if(!headersDone){firstHeaders??=at;head+=chunk;const end=head.indexOf('\r\n\r\n');if(end<0)return;chunk=head.slice(end+4);head=head.slice(0,end);headersDone=true;status=Number(head.match(/^HTTP\/\S+ (\d+)/)?.[1]);}if(chunk){firstBody??=at;stdout+=chunk;buffer+=chunk;if(onFrame)while(buffer.includes('\n\n')){const end=buffer.indexOf('\n\n'),frame=buffer.slice(0,end);buffer=buffer.slice(end+2);const data=frame.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n');if(data)onFrame(JSON.parse(data),at,frame);}}});child.stderr.on('data',b=>stderr+=b);child.on('close',(code,signal)=>resolvePromise({status,code,signal,stdout,stderr,headers:head,first_headers_ms:firstHeaders,first_body_ms:firstBody,total_ms:performance.now()-started}));});return {child,done,started};
}
async function sample(server,regime,index,existing){
 const key=randomUUID(),input=`BENCH_SAMPLE_${regime}_${index}: Reply with the synthetic benchmark text.`,start=performance.now();let agent=existing?.agent,turn,receipt,cursor=existing?.cursor??'0',firstText=null,firstTextValue,stream,rawFrames=[],calls=[];
 const observe=(value,at,frame)=>{rawFrames.push({at_ms:performance.now()-start,value});if(frame.includes('event: run')){receipt=value;agent=value.agent_id;turn=value.turn_id;}if(value.cursor)cursor=value.cursor;const message=value.message??value;if(message.type==='turn_accepted')turn??=message.id;if(message.type==='event'&&message.event?.type==='assistant.delta'&&typeof message.event.payload?.text==='string'&&message.event.payload.text.length>0&&value.turn_id===turn&&firstText===null){firstText=performance.now()-start;firstTextValue=message.event.payload.text;if(mode!=='stream')stream.child.kill();}};
 try{
  if((existing&&mode!=='stream')||mode==='legacy'){
   if(!existing){const client=curl(server,'/v1/agents','POST',{settings},{'Idempotency-Key':key});const result=await client.done;calls.push(result);assert.equal(result.status,201,result.stdout);agent=JSON.parse(result.stdout).agent_id;}
   turn=randomUUID();const client=curl(server,`/v1/agents/${agent}/turns`,'POST',{id:turn,input},{'Idempotency-Key':key});const result=await client.done;calls.push(result);assert.equal(result.status,202,result.stdout);receipt=JSON.parse(result.stdout);turn=receipt.turn_id??turn;
  }else if(mode==='combined'){
   const client=curl(server,'/v1/agent-runs','POST',{input,settings},{'Idempotency-Key':key});const result=await client.done;calls.push(result);assert.equal(result.status,201,result.stdout);receipt=JSON.parse(result.stdout);agent=receipt.agent_id;turn=receipt.turn_id;
  }
  stream=mode==='stream'?curl(server,existing?`/v1/agents/${agent}/turns`:'/v1/agent-runs','POST',existing?{input}:{input,settings},{'Idempotency-Key':key,Accept:'text/event-stream'},observe):curl(server,`/v1/agents/${agent}/events?cursor=${cursor}`,'GET',undefined,{Accept:'text/event-stream'},observe);
  const result=await stream.done;calls.push(result);assert.ok(!result.headers.includes('x-nanocodex-run-phases'),'internal timing header leaked');if(mode==='stream')assert.equal(result.code,0,'finite stream must EOF: '+result.stderr);assert.ok([200,201,202].includes(result.status),result.stdout);assert.notEqual(firstText,null,'No assistant text: '+result.stdout);assert.equal(firstTextValue,text);
  let completion;for(let n=0;n<200;n++){completion=await request(server,`/v1/agents/${agent}/turns/${turn}`);assert.equal(completion.status,200,JSON.stringify(completion));if(['completed','failed','cancelled'].includes(completion.value.state))break;await delay(10);}assert.equal(completion.value.state,'completed',JSON.stringify(completion));assert.match(JSON.stringify(completion.value),/BENCHMARK_ASSISTANT_TEXT/);
  const history=await request(server,`/v1/agents/${agent}/events/history?after=0&limit=256`);assert.equal(history.status,200);cursor=history.value.latest_cursor;
  const row={regime,index,process:server.process,agent,turn,key,input,ttft_ms:firstText,client_requests:calls.length,cursor,receipt,first_text:firstTextValue,stream_first_headers_ms:result.first_headers_ms,stream_first_body_ms:result.first_body_ms};rows.push(row);
  await writeFile(join(output,`${regime}-${index}.json`),JSON.stringify({row,calls,rawFrames,completion,history},null,2));await writeFile(join(output,'samples.json'),JSON.stringify(rows,null,2));console.log(`${label} ${regime}-${index}: ${firstText.toFixed(2)}ms`);return row;
 }catch(error){await writeFile(join(output,`${regime}-${index}-failure.json`),JSON.stringify({error:error.stack,agent,turn,receipt,rawFrames,calls},null,2));throw error;}finally{if(stream?.child.exitCode===null)stream.child.kill();}
}
async function verifyStreamContract(server){
 const checks=[];
 const check=async(name,path,method,body,status,headers={})=>{const result=await request(server,path,method,body,headers);assert.equal(result.status,status,name+': '+JSON.stringify(result));checks.push({name,...result});return result;};
 const seed=rows.findLast(row=>row.regime==='fresh_session'),body={input:seed.input,settings},headers={'Idempotency-Key':seed.key,Accept:'text/event-stream'};
 await check('unauthenticated cannot admit','/v1/agent-runs','POST',body,401,{...headers,authorization:'Bearer synthetic-invalid'});
 const scoped=(await request(server,'/__fixture','POST',{user:owner,capabilities:['agents:write','tools:use']})).value.token;
 await check('SSE requires read before admission','/v1/agent-runs','POST',body,403,{...headers,authorization:'Bearer '+scoped});
 await check('invalid resume cursor rejected before admission','/v1/agent-runs','POST',body,400,{...headers,'Last-Event-ID':'not-a-number'});
 await check('changed input preserves idempotency conflict','/v1/agent-runs','POST',{...body,input:body.input+' changed'},409,headers);
 const foreign=(await request(server,'/__fixture','POST',{user:other})).value.token;
 await check('other owner cannot read events',`/v1/agents/${seed.agent}/events`,'GET',undefined,404,{authorization:'Bearer '+foreign});
 await check('other owner cannot read turn',`/v1/agents/${seed.agent}/turns/${seed.turn}`,'GET',undefined,404,{authorization:'Bearer '+foreign});
 const before=providerCalls.length,frames=[];
 const replay=curl(server,'/v1/agent-runs','POST',body,headers,(value,at,frame)=>frames.push({value,at,frame}));const replayResult=await replay.done;
 assert.equal(replayResult.code,0,replayResult.stderr);assert.equal(replayResult.status,200,replayResult.stdout);assert.equal(frames[0].value.turn_id,seed.turn);assert.ok(frames.some(f=>f.value.type==='turn_completed'&&f.value.turn_id===seed.turn));assert.equal(providerCalls.length,before,'replay must not reinvoke model');assert.ok(!replayResult.headers.includes('x-nanocodex-run-phases'));
 checks.push({name:'same idempotency key replays receipt and durable terminal to EOF without reinference',result:replayResult,frames});
 const terminal=frames.find(f=>f.value.type==='turn_completed').value.cursor;
 const resumed=curl(server,'/v1/agent-runs','POST',body,{...headers,'Last-Event-ID':terminal});const resumedResult=await resumed.done;assert.equal(resumedResult.code,0);assert.equal(resumedResult.status,200);assert.ok(resumedResult.stdout.includes('event: run'));assert.ok(!resumedResult.stdout.includes('event: turn_completed'));checks.push({name:'resume after terminal returns receipt then EOF',result:resumedResult});
 const key=randomUUID(),disconnectFrames=[];let accepted;
 const disconnect=curl(server,'/v1/agent-runs','POST',{input:'BENCH_SAMPLE_disconnect: synthetic durability test.',settings},{'Idempotency-Key':key,Accept:'text/event-stream'},(value,at,frame)=>{disconnectFrames.push(value);if(frame.includes('event: run')){accepted=value;disconnect.child.kill();}});
 const detached=await disconnect.done;assert.ok(accepted?.turn_id,'disconnect receipt');
 const reconnectFrames=[];let reconnect;
 reconnect=curl(server,`/v1/agents/${accepted.agent_id}/events?cursor=${BigInt(accepted.accepted_cursor)-1n}`,'GET',undefined,{Accept:'text/event-stream'},value=>{reconnectFrames.push(value);if(value.type==='turn_completed'&&value.turn_id===accepted.turn_id)reconnect.child.kill();});
 const reconnected=await reconnect.done;assert.equal(reconnected.status,200);assert.ok(reconnectFrames.some(v=>v.type==='event'&&v.event?.type==='assistant.delta'&&v.turn_id===accepted.turn_id));assert.ok(reconnectFrames.some(v=>v.type==='turn_completed'&&v.turn_id===accepted.turn_id));
 const retained=await request(server,`/v1/agents/${accepted.agent_id}/turns/${accepted.turn_id}`);assert.equal(retained.value.state,'completed');checks.push({name:'disconnect after receipt leaves admitted turn durable and GET resumes through completion',accepted,detached,reconnected,reconnectFrames,retained});
 await verifyWarmStreamContract(server, checks, seed, scoped, foreign);
 await writeFile(join(output,'contract-checks.json'),JSON.stringify(checks,null,2));console.log('Streaming contract checks passed: '+checks.length);
}
async function verifyWarmStreamContract(server, checks, seed, scoped, foreign) {
 const path=`/v1/agents/${seed.agent}/turns`, accept={Accept:'text/event-stream'};
 const check=async(name,body,status,headers={})=>{
  const result=await request(server,path,'POST',body,headers);
  assert.equal(result.status,status,name+': '+JSON.stringify(result));
  checks.push({name,...result});return result;
 };
 const stream=async(name,body,status,headers={})=>{
  const frames=[];
  const result=await curl(server,path,'POST',body,{...accept,...headers},(value,at,frame)=>frames.push({value,at,frame})).done;
  assert.equal(result.code,0,name+': '+result.stderr);assert.equal(result.status,status,result.stdout);
  assert.match(result.headers,/content-type: text\/event-stream/i);
  assert.ok(!result.headers.includes('x-nanocodex-turn-created'));
  assert.ok(!result.headers.includes('x-nanocodex-turn-summary'));
  assert.match(frames[0]?.frame??'',/^event: run/);
  assert.equal(frames[0].value.agent_id,seed.agent);
  assert.ok(result.headers.includes('x-nanocodex-turn-id: '+frames[0].value.turn_id));
  checks.push({name,result,frames});return {result,frames,receipt:frames[0].value};
 };
 const waitDone=async id=>{
  for(let n=0;n<200;n++){
   const response=await request(server,path+'/'+id);
   assert.equal(response.status,200,JSON.stringify(response));
   if(response.value.state==='completed')return response;
   await delay(10);
  }
  assert.fail('turn did not complete: '+id);
 };
 // Every rejected new identity must be absent and must never reach the provider.
 const beforeRejected=providerCalls.length;
 for(const [name,headers,status] of [
  ['warm write-only cannot read stream',{...accept,authorization:'Bearer '+scoped},403],
  ['warm malformed cursor cannot admit',{...accept,'Last-Event-ID':'not-a-number'},400],
  ['warm wrong owner cannot admit',{...accept,authorization:'Bearer '+foreign},404],
 ]){
  const body={id:randomUUID(),input:'BENCH_SAMPLE_warm_rejected'};
  await check(name,body,status,headers);
  const missing=await request(server,path+'/'+body.id);assert.equal(missing.status,404);
  checks.push({name:name+' has no admitted receipt',id:body.id,...missing});
 }
 assert.equal(providerCalls.length,beforeRejected,'rejections must not invoke the provider');
 const jsonBody={id:randomUUID(),input:'BENCH_SAMPLE_warm_json'};
 let before=providerCalls.length;
 const jsonNew=await check('warm default JSON acceptance stays 202',jsonBody,202);
 assert.equal(jsonNew.value.turn_id,jsonBody.id);await waitDone(jsonBody.id);
 const jsonReplay=await check('warm JSON replay stays 200 with SSE q=0',jsonBody,200,{Accept:'text/event-stream;q=0'});
 assert.equal(jsonReplay.value.turn_id,jsonBody.id);assert.equal(providerCalls.length,before+1);
 // A key-only retry generates a different candidate internally: only the stored
 // receipt identity may select stopAfter or appear in response headers.
 const key=randomUUID(),keyBody={input:'BENCH_SAMPLE_warm_key_only'};
 before=providerCalls.length;
 const keyNew=await stream('warm key-only acceptance streams to terminal',keyBody,202,{'Idempotency-Key':key});
 const keyReplay=await stream('warm key-only replay keeps actual turn identity',keyBody,200,{'Idempotency-Key':key});
 assert.equal(keyNew.receipt.turn_id,keyReplay.receipt.turn_id);
 assert.equal(keyReplay.receipt.turn_idempotency_key,key);
 assert.ok(keyReplay.frames.some(f=>f.value.type==='turn_completed'&&f.value.turn_id===keyNew.receipt.turn_id));
 assert.equal(providerCalls.length,before+1,'key-only new + replay invokes model once');
 await check('warm changed key input conflicts',{input:keyBody.input+' changed'},409,{...accept,'Idempotency-Key':key});
 const idBody={id:randomUUID(),input:'BENCH_SAMPLE_warm_id_only'};
 before=providerCalls.length;
 const idNew=await stream('warm id-only acceptance has no fabricated key',idBody,202);
 const idReplay=await stream('warm id-only replay keeps identity without key',idBody,200);
 for(const {receipt} of [idNew,idReplay]){assert.equal(receipt.turn_id,idBody.id);assert.ok(!Object.hasOwn(receipt,'turn_idempotency_key'));}
 assert.equal(providerCalls.length,before+1,'id-only new + replay invokes model once');
 await check('warm changed id input conflicts',{...idBody,input:idBody.input+' changed'},409,accept);
 const terminal=idReplay.frames.find(f=>f.value.type==='turn_completed').value.cursor;
 const terminalReplay=await stream('warm terminal cursor returns only receipt then EOF',idBody,200,{'Last-Event-ID':terminal});
 assert.equal(terminalReplay.frames.length,1);
 // Subscription validation can fail after acceptance. The exact original key
 // still recovers that one turn and must not trigger a second provider request.
 const aheadKey=randomUUID(),aheadBody={input:'BENCH_SAMPLE_warm_cursor_ahead'};
 const summaryBefore=(await request(server,'/v1/agents')).value.summaries[seed.agent].turn_count;
 before=providerCalls.length;
 const ahead=await check('warm ahead cursor remains recoverable after admission',aheadBody,409,{...accept,'Idempotency-Key':aheadKey,'Last-Event-ID':'999999999'});
 assert.equal(ahead.value.error,'cursor_ahead');
 const recovered=await stream('warm cursor-ahead exact-key retry recovers admission',aheadBody,200,{'Idempotency-Key':aheadKey});
 assert.ok(recovered.frames.some(f=>f.value.type==='turn_completed'&&f.value.turn_id===recovered.receipt.turn_id));
 assert.equal(providerCalls.length,before+1);
 const afterAhead=(await request(server,'/v1/agents')).value.summaries[seed.agent];
 assert.equal(afterAhead.turn_count,summaryBefore+1,'failed subscription still updates account activity');
 checks.push({name:'warm cursor-ahead admission updates visible account turn count once',before:summaryBefore,after:afterAhead});
 const slots=[],slotControllers=[];
 const limitKey=randomUUID(),limitBody={input:'BENCH_SAMPLE_warm_subscriber_limit'};
 before=providerCalls.length;
 try {
  for(let i=0;i<32;i++) {
   const controller=new AbortController();slotControllers.push(controller);
   const response=await fetch(new URL(`/v1/agents/${seed.agent}/events`,server.base),{headers:{authorization:'Bearer '+server.token},signal:controller.signal});
   assert.equal(response.status,200);slots.push(response);
  }
  const limited=await check('warm full subscriber pool rejects stream after admission',limitBody,429,{...accept,'Idempotency-Key':limitKey});
  assert.equal(limited.value.error,'event_stream_limit');
 } finally {
  for(const controller of slotControllers)controller.abort();
  await Promise.allSettled(slots.map(response=>response.body.cancel()));
 }
 // Give loopback HTTP cancellation time to release retained subscription slots.
 await delay(100);
 const limitRecovered=await stream('warm subscriber-limit exact-key retry recovers one turn',limitBody,200,{'Idempotency-Key':limitKey});
 assert.ok(limitRecovered.frames.some(f=>f.value.type==='turn_completed'));
 assert.equal(providerCalls.length,before+1);
 const afterLimit=(await request(server,'/v1/agents')).value.summaries[seed.agent];
 assert.equal(afterLimit.turn_count,afterAhead.turn_count+1);
 checks.push({name:'warm subscriber-limit admission updates visible account count once',before:afterAhead.turn_count,after:afterLimit});
 if(family==='codex') {
  const grantHeaders={'x-nanocodex-connect-user':owner,'x-nanocodex-connect-grant-id':'0x'+'a'.repeat(64),
   'x-nanocodex-connect-capabilities':JSON.stringify(['agents:read','agents:write','tools:use']),
   'x-nanocodex-connect-connectors':'["chatgpt"]','x-nanocodex-connect-mcp-ids':'[]','content-type':'application/json',...accept};
  const connectBody={id:randomUUID(),input:'BENCH_SAMPLE_warm_connect'};
  before=providerCalls.length;
  for(const [name,override,status] of [
   ['Connect missing read rejected before admission',{'x-nanocodex-connect-capabilities':'["agents:write","tools:use"]'},403],
   ['Connect missing connector rejected before admission',{'x-nanocodex-connect-connectors':'[]'},403],
   ['Connect new streamed turn',{},202],['Connect streamed id replay',{},200],
  ]) {
   const response=await server.mf.dispatchFetch('https://nanocodex.internal'+path,{method:'POST',headers:{...grantHeaders,...override},body:JSON.stringify(connectBody)});
   const raw=await response.text();assert.equal(response.status,status,raw);
   if(status===403){assert.equal(providerCalls.length,before);assert.equal((await request(server,path+'/'+connectBody.id)).status,404);}
   else {assert.match(raw,/event: run/);assert.match(raw,/event: turn_completed/);assert.ok(raw.includes(connectBody.id));}
   checks.push({name,status:response.status,raw});
  }
  assert.equal(providerCalls.length,before+1,'Connect replay invokes provider only once');
  const projected=await server.mf.dispatchFetch('https://nanocodex.internal/v1/agents',{headers:grantHeaders});
  const listed=await projected.json();assert.equal(projected.status,200);assert.ok(!Object.hasOwn(listed.summaries[seed.agent],'may_have_scheduled_jobs'));
  checks.push({name:'Connect account-list read projection is preserved',summary:listed.summaries[seed.agent]});
 }
 const disconnectKey=randomUUID(),disconnectBody={input:'BENCH_SAMPLE_disconnect: warm durable turn'},disconnectFrames=[];
 before=providerCalls.length;let accepted;
 const disconnected=curl(server,path,'POST',disconnectBody,{...accept,'Idempotency-Key':disconnectKey},(value,at,frame)=>{
  disconnectFrames.push({value,at,frame});
  if(frame.includes('event: run')){accepted=value;disconnected.child.kill();}
 });
 const detached=await disconnected.done;
 assert.equal(detached.status,202);assert.ok(accepted?.turn_id);
 assert.ok(!disconnectFrames.some(f=>f.value.type==='turn_completed'||f.value.event?.type==='assistant.delta'),'receipt arrives before held provider output');
 const resumed=await stream('warm disconnect exact-key POST resumes the same durable turn',disconnectBody,200,{'Idempotency-Key':disconnectKey});
 assert.equal(resumed.receipt.turn_id,accepted.turn_id);
 assert.ok(resumed.frames.some(f=>f.value.type==='event'&&f.value.event?.type==='assistant.delta'&&f.value.turn_id===accepted.turn_id));
 assert.ok(resumed.frames.some(f=>f.value.type==='turn_completed'&&f.value.turn_id===accepted.turn_id));
 const retained=await waitDone(accepted.turn_id);assert.equal(providerCalls.length,before+1,'disconnect + reconnect invokes model once');
 checks.push({name:'warm receipt precedes provider output; cancelled subscription preserves durable work',accepted,detached,disconnectFrames,retained,provider_invocations:providerCalls.length-before});
}
function summarize(values){const a=values.toSorted((a,b)=>a-b);return a.length?{n:a.length,min:a[0],p50:a[Math.ceil(a.length*.5)-1],p95:a[Math.ceil(a.length*.95)-1],max:a.at(-1),mean:a.reduce((s,v)=>s+v,0)/a.length}:{n:0};}
let server;
try{
 for(let i=0;i<processCount;i++){server=await boot();try{await sample(server,'new_process_first_session',i);}finally{await server.mf.dispose();server=undefined;}}
 server=await boot();await sample(server,'shared_process_warmup',0);
 for(let i=0;i<count;i++){const fresh=await sample(server,'fresh_session',i);await sample(server,'warm_session',i,fresh);}
 if(mode==='stream')await verifyStreamContract(server);
 const summary={...config,completed_at:new Date().toISOString(),groups:Object.fromEntries(['new_process_first_session','fresh_session','warm_session'].map(regime=>[regime,summarize(rows.filter(r=>r.regime===regime).map(r=>r.ttft_ms))]))};await writeFile(join(output,'summary.json'),JSON.stringify(summary,null,2));console.log(JSON.stringify(summary.groups,null,2));
}catch(error){await writeFile(join(output,'failure.txt'),error.stack??String(error));throw error;}finally{if(server)await server.mf.dispose();await writeFile(join(output,'runtime.json'),JSON.stringify(runtime,null,2));await writeFile(join(output,'provider-calls.json'),JSON.stringify(providerCalls,null,2));}
