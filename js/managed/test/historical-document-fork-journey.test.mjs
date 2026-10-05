// Real managed HTTP, account authorization, broker, SQLite and shipped WASM.
// Fixtures bootstrap synthetic accounts, answer external model HTTP, and open
// the public document SDK only between full worker restarts (never beside a
// live managed agent). No fork/snapshot/seed behavior is substituted.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, writeFile, rm, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { builtinModules } from 'node:module';
import { createHash } from 'node:crypto';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const evidence = resolve(repo, process.env.NANOCODEX_DOCUMENT_FORK_EVIDENCE_DIR ?? 'output/historical-document-fork');
const owner = '11111111-1111-4111-8111-111111111155';
const other = '11111111-1111-4111-8111-111111111166';
const command = 'PATH=/srv/nanocodex/workspace/toolchain/node-v24.21.0-linux-x64/bin:$PATH node --test js/managed/test/historical-document-fork-journey.test.mjs';
const bootstrap = `
import managed, { DurableAgentSession } from './src/index.ts';
export * from './src/index.ts';
import { ensureAccount, createApiKey } from './src/account-auth.ts';
import { Agent, Claude, Transport } from '../nanocodex/host/index.mjs';
import module from '../nanocodex/pkg-web/nanocodex_bg.wasm';
import { createCloudflareDurabilityStore } from '../nanocodex/runtime/cloudflare-durability-store.mjs';
export class DocumentFixtureSession extends DurableAgentSession {
  fixtureUsed = false;
  async fetch(request) {
    if(new URL(request.url).pathname === '/__fixture/documents') {
      if(this.fixtureUsed) return Response.json({error:'restart_required'},{status:409});
      this.fixtureUsed = true;
      // The test disposes all workers before this request and after its result.
      // Acquiring the same store fences its old SDK owner as an extra safeguard.
      const {writes,keys} = await request.json();
      const stateId = this.ctx.storage.sql.exec('SELECT state_id FROM nanocodex_cloudflare_durability WHERE singleton=1').one().state_id;
      const sessionId = this.ctx.storage.sql.exec('SELECT session_id FROM nanocodex_cloudflare_agent WHERE singleton=1').one().session_id;
      const model = this.ctx.storage.sql.exec('SELECT model FROM managed_agent_settings WHERE singleton=1').one().model;
      const durability = createCloudflareDurabilityStore(this.ctx.storage);
      const noInference = () => {throw new Error('document fixture must never infer')};
      const agent = model.startsWith('claude-')
        ? await Claude.create({module,durability,durabilityId:stateId,model,tools:[],endpoint:'https://api.anthropic.com/v1/messages',auth:{apiKey:'synthetic-document-only'},fetch:noInference})
        : await Agent.create({module,durability,durabilityId:stateId,sessionId,model,tools:[],transport:Transport.openAi({apiKey:'synthetic-document-only',fetch:noInference,websocketWarmup:false})});
      try {
        if(writes) await agent.session.compareExchangeDocuments(writes);
        const documents={}; for(const key of keys??[]) documents[key]=await agent.session.document(key);
        return Response.json({documents});
      } catch(error) {return Response.json({error:String(error)},{status:409});}
      finally {await agent.session.shutdown();}
    }
    if(this.fixtureUsed) return Response.json({error:'restart_required'},{status:409});
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
  const fixture=path.startsWith('/__fixture/documents/') ? path.split('/') : null;
  if(fixture) return env.NANOCODEX_SESSIONS.getByName(fixture[3]).fetch(new Request('https://session.internal/__fixture/documents',request));
  return managed.fetch(request,env,ctx);
}};
`;
async function bundle(source,cwd,name) {
  const wasm=new Set();
  const output=await build({stdin:{contents:source,resolveDir:cwd},bundle:true,write:false,format:'esm',platform:'browser',target:'es2022',external:['cloudflare:*','node:*'],
    alias:{'node-rsa':resolve(repo,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'real-wasm',setup(b){
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
function claudeText(text,id) {
  const events=[{type:'message_start',message:{id,role:'assistant',model:'claude-sonnet-4-6',content:[],usage:{input_tokens:1,output_tokens:0}}},
    {type:'content_block_start',index:0,content_block:{type:'text',text}},{type:'content_block_stop',index:0},
    {type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage:{output_tokens:1}},{type:'message_stop'}];
  return new Response(events.map(event=>`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
}
test('historical managed forks retain typed checkpoints and isolated policy-selected documents beyond receipt retention', {timeout:1_200_000}, async t=>{
  await mkdir(evidence,{recursive:true});
  const trace=[],upstream=[],failures=[],outcomes=[],journalChecks=[];let mf,token,restarts=0,modelCalls=0;
  const provider=async request=>{
    try {
      const url=new URL(request.url);
      if(url.origin==='https://api.anthropic.com'&&url.pathname==='/v1/models') return Response.json({data:[{id:'claude-sonnet-4-6',display_name:'Synthetic Claude'}],has_more:false});
      if((url.origin==='https://api.openai.com'&&url.pathname==='/v1/responses')||(url.origin==='https://api.anthropic.com'&&url.pathname==='/v1/messages')) {
        const body=await request.json(),family=url.origin.includes('anthropic')?'claude':'codex';modelCalls++;
        if(body.model==='gpt-6-luna') return Response.json({id:'synthetic-title-'+modelCalls,output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'Historical document journey'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}});
        const encoded=JSON.stringify(body.messages??body.input);
        const child=encoded.includes('CHILD_ONLY');
        if(child){assert.match(encoded,/SELECTED_BOUNDARY/);assert.doesNotMatch(encoded,/LATER_BOUNDARY|BLOCKED_BOUNDARY/);}
        if(encoded.includes('GRANDCHILD_ONLY')) {assert.match(encoded,/CHILD_ONLY/);assert.doesNotMatch(encoded,/PARENT_AFTER_FORK/);}
        upstream.push({family,model:body.model,child,bytes:encoded.length,input:child||encoded.includes('GRANDCHILD_ONLY')?body.messages??body.input:undefined});
        let output=[{type:'message',role:'assistant',content:[{type:'output_text',text:'DOCUMENT_FORK_OK'}]}];
        if(family==='codex') {
          const lastUser=body.input.findLastIndex(row=>row.role==='user');
          const prompt=JSON.stringify(body.input[lastUser]);
          const cells=[
            ['SELECTED_BOUNDARY','store("historicalMemo",{count:1}); text("JOURNAL_SELECTED_1");'],
            ['LATER_BOUNDARY_0','store("historicalMemo",{count:2}); text("JOURNAL_LATER_2");'],
            ['CHILD_ONLY','const memo=load("historicalMemo"); if(memo?.count!==1)throw new Error("historical journal leaked latest state: "+JSON.stringify(memo)); store("historicalMemo",{count:42}); text("JOURNAL_CHILD_1_TO_42");'],
            ['CHILD_JOURNAL_READ','if(load("historicalMemo")?.count!==42)throw new Error("cold child journal lost branch write"); text("JOURNAL_CHILD_COLD_42");'],
            ['PARENT_JOURNAL_READ','if(load("historicalMemo")?.count!==2)throw new Error("child journal mutated parent"); text("JOURNAL_PARENT_2");'],
          ];
          const cell=cells.find(([marker])=>prompt.includes('"'+marker+'"'));
          if(cell) {
            const callId='historical-journal-'+cell[0];
            const result=body.input.slice(lastUser+1).find(row=>row.type==='custom_tool_call_output'&&row.call_id===callId);
            if(result) {
              assert.doesNotMatch(JSON.stringify(result),/historical journal leaked|cold child journal lost|child journal mutated|error_message/);
              assert.match(JSON.stringify(result),/JOURNAL_/,'real Code Mode cell must return its validated state marker');
              journalChecks.push(cell[0]);
            } else output=[{type:'custom_tool_call',call_id:callId,name:'exec',input:cell[1]}];
          }
        }
        const answer='DOCUMENT_FORK_OK';
        return family==='claude'?claudeText(answer,'message-'+modelCalls):new Response(`data: ${JSON.stringify({type:'response.completed',response:{id:'response-'+modelCalls,status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}})}\n\n`,{headers:{'content-type':'text/event-stream'}});
      }
      const response=await claudeProvider(request);if(response)return response;
      return new Response('unavailable synthetic external dependency',{status:502});
    }catch(error){failures.push(String(error));throw error;}
  };
  const managed=await bundle(bootstrap,resolve(repo,'js/managed'),'managed');
  const egress=await bundle(`export * from './src/egress.ts'; export {default} from './src/egress.ts';`,resolve(repo,'js/egress'),'egress');
  const persistence=resolve(evidence,'sqlite-'+crypto.randomUUID());
  const date='2026-07-29',flags=['nodejs_compat','enable_request_signal'];
  const options={durableObjectsPersist:persistence,r2Persist:resolve(persistence,'r2'),workers:[
    {name:'managed',modulesRoot:'/',modules:managed,compatibilityDate:date,compatibilityFlags:flags,bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},
      serviceBindings:{NANOCODEX:'egress',NANOCODEX_SESSION_MODEL_EGRESS:{name:'egress',entrypoint:'SessionModelEgress'}},
      durableObjects:Object.fromEntries([['NANOCODEX_AUTH','NonceStorage'],['NANOCODEX_USERS','UserAccount'],['NANOCODEX_ORGANIZATIONS','Organization'],['NANOCODEX_API_KEYS','ApiKeyRecord'],['NANOCODEX_SESSIONS','DocumentFixtureSession'],['NANOCODEX_ACCOUNT_TOOLS','AccountHostedTools'],['NANOCODEX_VM_HOST_POOLS','VmHostPool'],['NANOCODEX_MEMORY','MemoryScope']].map(([binding,className])=>[binding,{className,useSQLite:true}])),r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],outboundService:'external-provider'},
    {name:'egress',modulesRoot:'/',modules:egress,compatibilityDate:date,compatibilityFlags:flags,bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},serviceBindings:{MANAGED_AGENT_OWNERSHIP:{name:'managed',entrypoint:'ManagedAgentOwnership'}},
      durableObjects:Object.fromEntries([['USER_CREDENTIALS','UserCredentialBroker'],['AGENT_SUBJECTS','AgentSubjectDirectory'],['USER_CONNECTORS','UserConnectorBroker'],['MCP_CONNECTIONS','McpConnectionDirectory'],['SPOTIFY_RATE_LIMITS','SpotifyRateLimit'],['GMAIL_PUSH_MAILBOXES','GmailPushMailbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),outboundService:'external-provider'},
    {name:'external-provider',script:externalProvider,modules:true,compatibilityDate:date,compatibilityFlags:flags,serviceBindings:{CONTROL:provider}}]};
  const restart=async()=>{await mf?.dispose();mf=new Miniflare(options);restarts++;};
  const call=async(path,method='GET',body,status=200,headers={})=>{
    const response=await mf.dispatchFetch('https://nanocodex.example'+path,{method,headers:{authorization:'Bearer '+token,'content-type':'application/json',origin:'https://nanocodex.example',...headers},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const text=await response.text();let value;try{value=JSON.parse(text);}catch{value=text;}
    trace.push({path,method,status:response.status,value:path==='/__fixture'||path.includes('/credentials/claude/login')?{private_fields:'redacted',state:value?.state}:value});
    assert.equal(response.status,status,`${method} ${path}: ${text}`);return value;
  };
  const turn=async(agent,input,id)=>{
    const receipt=await call(`/v1/agents/${agent}/turns`,'POST',{id,input},202);
    let result;for(let n=0;n<500;n++){result=await call(`/v1/agents/${agent}/turns/${receipt.turn_id??id}`);if(['completed','failed','cancelled'].includes(result.state))break;await new Promise(r=>setTimeout(r,10));}
    assert.equal(result.state,'completed',JSON.stringify(result));assert.match(JSON.stringify(result),/DOCUMENT_FORK_OK/);return result;
  };
  const docs=async(agent,writes,keys=['initial','current','asOf','late','block'])=>{
    await restart();try{return await call('/__fixture/documents/'+agent,'POST',{writes,keys});}finally{await restart();}
  };
  const fork=async(agent,key,at,status=201,headers={})=>call(`/v1/agents/${agent}/forks`,'POST',at?{at}:undefined,status,{'idempotency-key':key,...headers});
  const write=(key,expectedVersion,value,fork='asOf')=>({key,expectedVersion,value,fork});
  try {
    await restart();token=(await call('/__fixture','POST',{user:owner})).token;
    await call('/__fixture/openai','POST',undefined,204);
    const login=await call('/v1/credentials/claude/login','POST');
    await call('/v1/credentials/claude/login/complete','POST',{code:'managed-runtime#'+new URL(login.authorization_url).searchParams.get('state')});
    const scoped=(await call('/__fixture','POST',{user:owner,capabilities:['agents:read','agents:write']})).token;
    const stranger=(await call('/__fixture','POST',{user:other})).token;
    for(const family of ['codex','claude']) {
      const model=family==='claude'?'claude-sonnet-4-6':'gpt-6.1-sol';
      const parent=(await call('/v1/agents','POST',{settings:{model,thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
      await turn(parent,'PRE_SEED',family+'-pre-seed');
      await docs(parent,['initial','current','asOf'].map(policy=>write(policy,0,1,policy)));
      const selected=family+'-selected';await turn(parent,'SELECTED_BOUNDARY',selected);
      await docs(parent,['initial','current','asOf'].map(policy=>write(policy,1,2,policy)));
      // Shipped managed retention is 512. Ordinary zero-document-write turns
      // must preserve SELECTED_BOUNDARY even after its terminal receipt expires.
      for(let i=0;i<513;i++) {
        await turn(parent,'LATER_BOUNDARY_'+i,family+'-later-'+i);
        if(i%64===0) console.info('DOCUMENT_FORK_PROGRESS',{family,completedLaterTurns:i+1});
      }
      await docs(parent,[write('late',0,'omitted')]);
      const beforeCalls=modelCalls;
      await fork(parent,family+'-denied-scope',selected,403,{authorization:'Bearer '+scoped});
      await fork(parent,family+'-denied-owner',selected,404,{authorization:'Bearer '+stranger});
      await fork(parent,family+'-denied-anonymous',selected,401,{authorization:''});
      await fork(parent,family+'-denied-connect',selected,401,{
        authorization:'','x-nanocodex-connect-user':owner,'x-nanocodex-connect-grant-id':'0x'+'a'.repeat(64),
        'x-nanocodex-connect-capabilities':JSON.stringify(['agents:read','agents:write','tools:use']),
        'x-nanocodex-connect-connectors':JSON.stringify(['chatgpt']),'x-nanocodex-connect-mcp-ids':'[]'});
      assert.equal(modelCalls,beforeCalls,'denied fork performs no inference');
      await fork(parent,family+'-invalid',{bad:'selector'},400);
      await fork(parent,family+'-missing','unknown-operation',409);
      await restart();
      const child=(await fork(parent,family+'-historical',selected)).agent_id;
      await restart();
      assert.equal((await fork(parent,family+'-historical',selected)).agent_id,child,'cold replay retains one fork');
      await fork(parent,family+'-historical',family+'-later-0',409);
      await turn(child,'CHILD_ONLY',family+'-child-turn');
      const inherited=(await docs(child)).documents;
      assert.deepEqual(['initial','current','asOf'].map(key=>inherited[key].value),[1,2,1]);
      assert.equal(inherited.late,null);assert.equal(inherited.block,null);
      await docs(child,[write('asOf',inherited.asOf.version,42)]);
      assert.equal((await docs(child)).documents.asOf.value,42,'cold branch value retained');
      assert.equal((await docs(parent)).documents.asOf.value,2,'branch writes leave parent unchanged');
      if(family==='codex') {
        await turn(child,'CHILD_JOURNAL_READ',family+'-child-journal-cold');
        await restart();
        await turn(parent,'PARENT_JOURNAL_READ',family+'-parent-journal-cold');
        assert.deepEqual(journalChecks,['SELECTED_BOUNDARY','LATER_BOUNDARY_0','CHILD_ONLY','CHILD_JOURNAL_READ','PARENT_JOURNAL_READ']);
      }
      await docs(parent,[write('block',0,'private','block')]);
      await turn(parent,'BLOCKED_BOUNDARY',family+'-blocked');await restart();
      await fork(parent,family+'-blocked-fork',family+'-blocked',409);
      await fork(parent,family+'-blocked-historical',selected,409);
      assert.equal((await fork(parent,family+'-historical',selected)).agent_id,child,'existing accepted fork replay remains stable after source is blocked');
      outcomes.push({family,parent,child,selected,completedParentTurns:family==='codex'?517:516,journalAtSelectedOperation:family==='codex'?{selected:1,later:2,coldChild:42,parent:2}:null,terminalReceiptRetention:512,selectedValues:[1,2,1],coldChildValue:42,parentValue:2,lateOmitted:true,blockRejected:true,coldReplay:true,authorizationFailClosed:true});
    }
    assert.deepEqual(failures,[]);t.diagnostic(JSON.stringify({outcomes,restarts,modelCalls,journalChecks}));
  }finally {
    await mf?.dispose();
    await writeFile(resolve(evidence,'public-api-trace.json'),JSON.stringify(trace,null,2));
    await writeFile(resolve(evidence,'provider-trace.json'),JSON.stringify(upstream,null,2));
    await writeFile(resolve(evidence,'result.json'),JSON.stringify({command,outcomes,restarts,modelCalls,journalChecks,failures,wasmSha256:createHash('sha256').update(await readFile(resolve(repo,'js/nanocodex/pkg-web/nanocodex_bg.wasm'))).digest('hex')},null,2));
    await rm(persistence,{recursive:true,force:true});
  }
});
