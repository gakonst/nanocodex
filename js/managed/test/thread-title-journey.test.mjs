// Reproduce: pnpm --filter nanocodex-managed-service run test:thread-title
// Real HTTP, managed Worker/auth/account registry, SQLite Session DO and WASM.
// Fixtures replace only account bootstrap and external model/OAuth providers.
// Requires the same generated WASM and egress assets as test:claude-managed.
// Per-run public API/provider evidence is retained in ignored output/.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, writeFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { build } from 'esbuild';
import { builtinModules } from 'node:module';
import { Miniflare, Response as FixtureResponse, WebSocketPair } from 'miniflare';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const evidence = resolve(repo, process.env.NANOCODEX_THREAD_TITLE_EVIDENCE_DIR ?? 'output/thread-title-journey/'+Date.now()+'-'+process.pid);
const identity = '11111111-1111-4111-8111-111111111133';
const bootstrap = `
import { DurableObject, WorkerEntrypoint } from 'cloudflare:workers';
export class FixtureAi extends WorkerEntrypoint {
  async run(model, input) {
    const response = await fetch('https://title-fixture.invalid/run', {method:'POST', body:JSON.stringify({model,input})});
    if (!response.ok) throw new Error('Synthetic naming provider unavailable');
    return response.json();
  }
}
// No container is allocated; session deletion still checks legacy resources.
export class FixtureSandbox extends DurableObject {
  async clearRemoteDesktop() {}
  async destroy() {}
}
import managed from './src/index.ts';
export * from './src/index.ts';
import { ensureAccount, createApiKey } from './src/account-auth.ts';
export default { async fetch(request, env, ctx) {
  if (new URL(request.url).pathname === '/__fixture/openai') {
    return env.NANOCODEX.fetch('https://broker.internal/users/11111111-1111-4111-8111-111111111133/credentials/openai', {method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({api_key:'sk-synthetic-openai-runtime'})});
  }
  if (new URL(request.url).pathname === '/__fixture') {
    const { user, capabilities } = await request.json();
    await ensureAccount(env, user, true);
    const auth = await (await env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env, { kind:'api_key',userId:user,...auth.grant,
      ...(capabilities?{capabilities}:{}),subjectId:'fixture:'+user,credentialId:'fixture' }, 'synthetic-thread-title'));
  }
  return managed.fetch(request, env, ctx);
} };
`;
async function bundle(source, cwd, name) {
  const wasm = new Set();
  const output = await build({ stdin: { contents:source, resolveDir:cwd }, bundle:true, write:false,
    format:'esm', platform:'browser', target:'es2022', external:['cloudflare:*','node:*'],
    alias:{'node-rsa':resolve(repo,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},
    plugins:[{ name:'actual-wasm', setup(b) {
      b.onResolve({filter:/^[a-z][a-z_]*(?:\/[a-z_]+)?$/}, args => builtinModules.includes(args.path) ? {path:'node:'+args.path,external:true} : undefined);
      b.onResolve({filter:/\.wasm$|^nanocodex\/wasm$/}, args => {
        const path = args.path === 'nanocodex/wasm' ? resolve(repo,'js/nanocodex/pkg-web/nanocodex_bg.wasm') : resolve(args.resolveDir,args.path);
        wasm.add(path); return {path,external:true};
      });
    } }],
  });
  const path = resolve(evidence,`${name}.mjs`);
  const code = output.outputFiles[0].text;
  const requires = [...new Set([...code.matchAll(/__require\("(node:[^"]+)"\)/g)].map(match=>match[1]))];
  const prelude = requires.map((name,index)=>`import * as builtin${index} from ${JSON.stringify(name)};`).join('\n')
    + `\nconst requireMap={${requires.map((name,index)=>`${JSON.stringify(name)}:builtin${index}`).join(',')}}; const require=name=>{if(!requireMap[name])throw new Error('Unexpected require '+name);return requireMap[name];};\n`;
  await writeFile(path,prelude+code);
  return [{type:'ESModule',path},...Array.from(wasm,path=>({type:'CompiledWasm',path}))];
}
function claudeReply(id) {
  const events = [
    {type:'message_start',message:{id,role:'assistant',model:'claude-sonnet-4-6',content:[],usage:{input_tokens:10,output_tokens:0}}},
    {type:'content_block_start',index:0,content_block:{type:'text',text:'MAIN_TURN_OK: ready for the next step.'}},
    {type:'content_block_stop',index:0},
    {type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage:{output_tokens:2}},
    {type:'message_stop'},
  ];
  return new Response(events.map(event=>`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
}

test('automatic GLM titles survive restart across GPT and Claude, and recover from naming failures', {timeout:180_000}, async () => {
  await mkdir(evidence,{recursive:true});
  const trace=[], naming=[], main=[], errors=[];
  let mf, token, base;
  const failures=new Map();
  const opening = 'Please help with the task described after this introductory sentence: GPT_TITLE Fix the calendar export';
  const gptInput = '<environment_context>\n<cwd>/synthetic/workspace</cwd>\n<shell>javascript</shell>\n</environment_context>\n' + opening;
  let releaseTitle;
  const titleGate = new Promise(resolve => { releaseTitle = resolve; });
  const provider=async request=> {
    try {
      const url=new URL(request.url);
      if(url.hostname==='title-fixture.invalid') {
        const {model,input}=await request.json();
        assert.equal(model,'@cf/zai-org/glm-5.3');
        assert.equal(input.reasoning_effort,'low');
        // Titles carry no artificial output ceiling; GLM reasoning needs the room.
        assert.equal(input.max_completion_tokens,undefined);
        const source=JSON.stringify(input.messages.filter(message=>message.role==='user'));
        naming.push({model,input});
        if (source.includes('GPT_TITLE')) assert.equal(input.messages.filter(message=>message.role==='user')[0].content, opening,
          'GLM receives the opening request beyond the 56-character preview, without the client context');
        if (source.includes('DELAYED_TITLE')) await titleGate;
        const scenario=[...failures.keys()].find(key=>source.includes(key));
        if(scenario && failures.get(scenario)!=='recovered') {
          if(scenario==='OUTAGE')return new Response('synthetic outage',{status:503});
          if(scenario==='MALFORMED')return Response.json({unexpected:'no generated text'});
          if(scenario==='INCOMPLETE')return Response.json({choices:[{finish_reason:'length',message:{content:'Truncated title'}}]});
          const content=scenario==='WORD_LIMIT'?'One two three four five six':'x'.repeat(57);
          return Response.json({choices:[{finish_reason:'stop',message:{content}}]});
        }
        return Response.json({choices:[{finish_reason:'stop',message:{content:scenario?'Recover session naming':source.includes('GPT_TITLE')?'Fix calendar export':'Plan coastal trip'}}]});
      }
      if(url.origin==='https://api.anthropic.com' && url.pathname==='/v1/models')
        return Response.json({data:[{id:'claude-sonnet-4-6',display_name:'Claude Sonnet'}],has_more:false});
      if(url.origin==='https://api.anthropic.com' && url.pathname==='/v1/messages') {
        const body=await request.json();assert.equal(body.model,'claude-sonnet-4-6');
        main.push({provider:'claude',model:body.model});
        return claudeReply('claude-'+main.length);
      }
      if(url.origin==='https://api.openai.com' || url.origin==='https://chatgpt.com') {
        assert.equal(url.origin,'https://api.openai.com');
        assert.equal(request.headers.get('authorization'),'Bearer sk-synthetic-openai-runtime');
        const reply=body=> {
          assert.equal(body.model,'gpt-6.1-sol','naming must never borrow OpenAI credentials');
          main.push({provider:'openai',model:body.model});
          return {type:'response.completed',response:{id:'gpt-'+main.length,status:'completed',end_turn:true,
            output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'MAIN_TURN_OK: calendar export repaired.'}]}],
            usage:{input_tokens:10,output_tokens:2,total_tokens:12}}};
        };
        if(request.headers.get('upgrade')==='websocket') {
          const [client,server]=Object.values(new WebSocketPair());server.accept();
          server.addEventListener('close',()=>server.close(1000));
          server.addEventListener('message',event=>{try {server.send(JSON.stringify(reply(JSON.parse(event.data))));}
            catch(error){errors.push(String(error));server.close(1011,'fixture mismatch');}});
          return new FixtureResponse(null,{status:101,webSocket:client});
        }
        return new Response('data: '+JSON.stringify(reply(await request.json()))+'\n\n',{headers:{'content-type':'text/event-stream'}});
      }
      // The GPT runtime discovers public MCP catalogs; no tool execution is needed.
      if(['developers.openai.com','mcp.tempo.xyz','mercator.sh','docs.mcp.cloudflare.com','viem.sh','vocs.dev'].includes(url.hostname)) {
        if(request.method!=='POST')return new Response(null,{status:405});
        const body=await request.json();
        if(body.method==='notifications/initialized')return new Response(null,{status:202});
        return Response.json({jsonrpc:'2.0',id:body.id,result:body.method==='initialize'
          ?{protocolVersion:body.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'title-journey',version:'1'}}:{tools:[]}});
      }
      const response=await claudeProvider(request);if(response)return response;
      throw new Error('Unexpected external request '+url.origin+url.pathname);
    } catch(error) {errors.push(String(error));return new Response(String(error),{status:502});}
  };
  const managedModules=await bundle(bootstrap,resolve(repo,'js/managed'),'managed');
  const egressModules=await bundle(`export * from './src/egress.ts'; export {default} from './src/egress.ts';`,resolve(repo,'js/egress'),'egress');
  const persistence=resolve(evidence,'sqlite');
  const options={port:0,durableObjectsPersist:persistence,r2Persist:resolve(persistence,'r2'),workers:[
    {name:'managed',modulesRoot:'/',modules:managedModules,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],
      bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},
      serviceBindings:{AI:{name:'managed',entrypoint:'FixtureAi'},NANOCODEX:'egress',NANOCODEX_SESSION_MODEL_EGRESS:{name:'egress',entrypoint:'SessionModelEgress'}},
      durableObjects:Object.fromEntries([['NANOCODEX_AUTH','NonceStorage'],['NANOCODEX_USERS','UserAccount'],['NANOCODEX_ORGANIZATIONS','Organization'],['NANOCODEX_API_KEYS','ApiKeyRecord'],['NANOCODEX_SESSIONS','DurableAgentSession'],['NANOCODEX_ACCOUNT_TOOLS','AccountHostedTools'],['NANOCODEX_VM_HOST_POOLS','VmHostPool'],['NANOCODEX_MEMORY','MemoryScope'],['NANOCODEX_SANDBOXES','FixtureSandbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),
      r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],outboundService:provider},
    {name:'egress',modulesRoot:'/',modules:egressModules,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],
      bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},
      serviceBindings:{MANAGED_AGENT_OWNERSHIP:{name:'managed',entrypoint:'ManagedAgentOwnership'}},
      durableObjects:Object.fromEntries([['USER_CREDENTIALS','UserCredentialBroker'],['AGENT_SUBJECTS','AgentSubjectDirectory'],['USER_CONNECTORS','UserConnectorBroker'],['MCP_CONNECTIONS','McpConnectionDirectory'],['SPOTIFY_RATE_LIMITS','SpotifyRateLimit'],['GMAIL_PUSH_MAILBOXES','GmailPushMailbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),outboundService:provider},
  ]};
  const open=async()=>{mf=new Miniflare(options);base=await mf.ready;};
  const call=async(path,method='GET',body,expected=200)=>{
    const response=await fetch(new URL(path,base),{method,headers:{...(token?{authorization:'Bearer '+token}:{}),'content-type':'application/json'},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const raw=await response.text();let value;try{value=JSON.parse(raw);}catch{value=raw;}
    if(trace.length<1000)trace.push({path,method,status:response.status,value:path.startsWith('/__fixture')||path.includes('/login')?'synthetic credentials redacted':value});
    assert.equal(response.status,expected,method+' '+path+': '+raw);return value;
  };
  const create=async model=>(await call('/v1/agents','POST',{settings:{model,thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
  const summary=async id=>(await call('/v1/agents')).summaries[id];
  const turn=async(id,input,turnId)=>{
    await call(`/v1/agents/${id}/turns`,'POST',{input,id:turnId},202);
    let result;const deadline=Date.now()+30_000;
    do {result=await call(`/v1/agents/${id}/turns/${turnId}`);if(['completed','failed','cancelled'].includes(result.state))break;await new Promise(r=>setTimeout(r,40));}while(Date.now()<deadline);
    assert.equal(result.state,'completed',JSON.stringify(result));assert.match(JSON.stringify(result),/MAIN_TURN_OK/);
  };
  const titled=async(id,title)=>{
    let value;const deadline=Date.now()+10_000;
    do {value=await summary(id);if(value.presentation?.title===title)break;await new Promise(r=>setTimeout(r,40));}while(Date.now()<deadline);
    assert.equal(value.presentation?.title,title,JSON.stringify(value));
    assert.equal(value.title,title,'Resume reads the canonical summary title');
    assert.ok(title.length<=56 && title.split(' ').length<=5);return value;
  };
  try {
    await open();token=(await call('/__fixture','POST',{user:identity})).token;
    const login=await call('/v1/credentials/claude/login','POST');
    await call('/v1/credentials/claude/login/complete','POST',{code:'managed-runtime#'+new URL(login.authorization_url).searchParams.get('state')});
    const credentials=await call('/v1/credentials');assert.equal(credentials.openai.connected,false);assert.equal(credentials.chatgpt.connected,false);
    const claude=await create('claude-sonnet-4-6');
    await turn(claude,'CLAUDE_TITLE Help plan a coastal trip','claude-first');await titled(claude,'Plan coastal trip');
    assert.equal(main.filter(row=>row.provider==='openai').length,0,'Claude naming succeeds without any OpenAI credential');
    await call('/__fixture/openai','POST',undefined,204);
    const gpt=await create('gpt-6.1-sol');
    await turn(gpt,gptInput,'gpt-first');await titled(gpt,'Fix calendar export');
    assert.equal(naming.length,2,'exactly one title per initial thread');
    await mf.dispose();await open();
    await titled(claude,'Plan coastal trip');await titled(gpt,'Fix calendar export');
    for(const [id,input,turnId] of [[claude,'CLAUDE_TITLE Help plan a coastal trip','claude-first'],[gpt,gptInput,'gpt-first']])
      await call(`/v1/agents/${id}/turns`,'POST',{input,id:turnId},200);
    await turn(claude,'Continue with the itinerary','claude-resume');await turn(gpt,'Continue with export validation','gpt-resume');
    await titled(claude,'Plan coastal trip');await titled(gpt,'Fix calendar export');assert.equal(naming.length,2,'retained title is not regenerated by replay or new turns');
    const delayed=await create('claude-sonnet-4-6');
    await turn(delayed,'DELAYED_TITLE Plan a coastal trip','delayed-first');
    assert.equal((await summary(delayed)).presentation?.title,undefined,'main turn completes while title generation is still pending');
    releaseTitle();await titled(delayed,'Plan coastal trip');
    const failed=[];
    for(const marker of ['OUTAGE','MALFORMED','INCOMPLETE','WORD_LIMIT','CHAR_LIMIT']) {
      failures.set(marker,'failing');const id=await create('claude-sonnet-4-6');
      await turn(id,marker+' Help organize a conference schedule',marker+'-first');
      // Drain provider work before inspecting the public fallback and restarting.
      const deadline=Date.now()+10_000;
      while(!naming.some(row=>JSON.stringify(row.input.messages).includes(marker)) && Date.now()<deadline)await new Promise(r=>setTimeout(r,40));
      assert.ok(naming.some(row=>JSON.stringify(row.input.messages).includes(marker)));
      const value=await summary(id);assert.equal(value.presentation?.title,undefined);assert.ok(value.title?.includes(marker),'prompt-derived fallback remains visible');
      failed.push({id,marker,fallback:value.title});
    }
    await mf.dispose();await open();
    for(const {id,marker,fallback} of failed) {
      const value=await summary(id);assert.equal(value.title,fallback);assert.equal(value.presentation?.title,undefined);
      failures.set(marker,'recovered');await turn(id,marker+' Retry organizing the schedule',marker+'-retry');await titled(id,'Recover session naming');
    }
    assert.equal(naming.length,13,'three successful titles plus five failed attempts and five later recoveries');
    for (const {marker} of failed) {
      const sources=naming.filter(row=>JSON.stringify(row.input.messages).includes(marker)).map(row=>row.input.messages.filter(message=>message.role==='user'));
      assert.deepEqual(sources[1],sources[0],'retry after restart uses the original source, not the later follow-up');
    }
    assert.deepEqual(errors,[]);
    console.info('THREAD_TITLE_JOURNEY',{namingRequests:naming.length,mainTurns:main.length,restarts:2,providers:['Claude without OpenAI credentials','GPT'],failures:[...failures.keys()],evidence});
  } finally {
    await mf?.dispose();
    await writeFile(resolve(evidence,'public-api-trace.json'),JSON.stringify(trace,null,2));
    await writeFile(resolve(evidence,'provider-trace.json'),JSON.stringify({naming,main,errors},null,2));
    await rm(persistence,{recursive:true,force:true});
  }
});
