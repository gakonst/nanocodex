// Reproduce: node --test js/managed/test/company-context-journey.test.mjs
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
import { Miniflare } from 'miniflare';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';
import { fetch } from "./support/miniflare-fetch.mjs";
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const evidence = resolve(repo, process.env.NANOCODEX_COMPANY_CONTEXT_EVIDENCE_DIR ?? 'output/company-context-journey/'+Date.now()+'-'+process.pid);
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
function sse(block, stop, id, newline = "\n", terminal = true) {
  const tool = block.type === 'tool_use';
  const events = [
    {type:'message_start',message:{id,role:'assistant',model:'claude-sonnet-4-6',content:[],usage:{input_tokens:10,output_tokens:0}}},
    {type:'content_block_start',index:0,content_block:tool?{type:'tool_use',id:block.id,name:['web_search','code_execution','text_editor','computer'].includes(block.name.toLowerCase())?block.name:'_'+block.name,input:{}}:block},
    ...(tool?[{type:'content_block_delta',index:0,delta:{type:'input_json_delta',partial_json:JSON.stringify(block.input)}}]:[]),
    {type:'content_block_stop',index:0},
    {type:'message_delta',delta:{stop_reason:stop,stop_sequence:null},usage:{output_tokens:2}},
    {type:'message_stop'},
  ];
  return new Response(events.filter(e=>terminal || e.type!=='message_stop').map(e=>`event: ${e.type}${newline}data: ${JSON.stringify(e)}${newline}${newline}`).join(''),{headers:{'content-type':'text/event-stream'}});
}
test('managed company context: private defaults, team contributions, reader and revocation fences', {timeout:180_000}, async () => {
  await mkdir(evidence,{recursive:true});
  const trace=[], naming=[], main=[], errors=[];
  let mf, token, base, teamIdentifier;
  const receipts=[];
  const provider=async request=> {
    try {
      const url=new URL(request.url);
      if(url.hostname==='title-fixture.invalid') {
        const {model,input}=await request.json();
        naming.push({model,input});
        return Response.json({choices:[{finish_reason:'stop',message:{content:'Plan synthetic project'}}]});
      }
      if(url.origin==='https://api.anthropic.com' && url.pathname==='/v1/models')
        return Response.json({data:[{id:'claude-sonnet-4-6',display_name:'Claude Sonnet'}],has_more:false});
      if(url.origin==='https://api.anthropic.com' && url.pathname==='/v1/messages') {
        const body=await request.json();assert.equal(body.model,'claude-sonnet-4-6');
        main.push({provider:'claude',model:body.model});
        const result=body.messages.at(-1)?.content?.find?.(block=>block.type==='tool_result');
        if(result) {
          receipts.push(result);
          assert.equal(!!result.is_error,result.tool_use_id.includes('denied'),JSON.stringify(result));
          return sse({type:'text',text:'MAIN_TURN_OK: tool completed.'},'end_turn','done-'+main.length);
        }
        const prompt=JSON.stringify(body.messages.at(-1));
        const shared=prompt.includes('SHARED_MAPLE_295');
        if (shared) assert.doesNotMatch(JSON.stringify(body), /PRIVATE_ORCHID_794/, 'shared model context excludes personal memory and history');
        const lookup=prompt.includes('LOOKUP_TEAM_MEMORY');
        const history=prompt.includes('LOOKUP_TEAM_HISTORY');
        const discover=prompt.includes('DISCOVER_CONTEXTS');
        const name=discover?'list_company_context':lookup?'memories__read':history?'find_session':'memories__write';
        const input=discover?{}:lookup?{path:'MEMORY.md',team_id:teamIdentifier}:history?{query:'SHARED_MAPLE_295',team_id:teamIdentifier}:{operation:'put',path:'MEMORY.md',content:shared?'SHARED_MAPLE_295 team memory':'PRIVATE_ORCHID_794 personal memory'};
        // Code Mode is mandatory since eda4a21e3: the model reaches context tools only through exec.
        const code='text(JSON.stringify(await tools.'+name+'('+JSON.stringify(input)+')));';
        return sse({type:'tool_use',id:(prompt.includes('DENIED_CONTEXT')?'denied-':'context-tool-')+main.length,name:'exec',input:{code}},'tool_use','call-'+main.length);
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
      bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true',NANOCODEX_ADMIN_USER_ID:'11111111-1111-4111-8111-111111111135'},
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
  const create=async scope=>(await call('/v1/agents','POST',{...(scope?{scope}:{}),settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
  const turn=async(id,input,turnId)=>{
    await call(`/v1/agents/${id}/turns`,'POST',{input,id:turnId},202);
    let result;const deadline=Date.now()+30_000;
    do {result=await call(`/v1/agents/${id}/turns/${turnId}`);if(['completed','failed','cancelled'].includes(result.state))break;await new Promise(r=>setTimeout(r,40));}while(Date.now()<deadline);
    assert.equal(result.state,'completed',JSON.stringify(result));assert.match(JSON.stringify(result),/MAIN_TURN_OK/);
  };
  try {
    await open();
    const alice=(await call('/__fixture','POST',{user:identity})).token;
    const bobId='11111111-1111-4111-8111-111111111134';
    const bob=(await call('/__fixture','POST',{user:bobId})).token;
    const admin=(await call('/__fixture','POST',{user:'11111111-1111-4111-8111-111111111135'})).token;
    token=alice;
    const login=await call('/v1/credentials/claude/login','POST');
    await call('/v1/credentials/claude/login/complete','POST',{code:'managed-runtime#'+new URL(login.authorization_url).searchParams.get('state')});
    const team=await call('/v1/teams','POST',{name:'Synthetic Company'},201);
    teamIdentifier=team.id;
    const tp='/v1/teams/'+team.id;
    const invite=await call(tp+'/invitations','POST',{role:'writer',user_id:bobId},201);
    token=bob;await call(tp+'/invitations/accept','POST',{token:invite.token});
    token=alice;
    const privateId=await create();
    await turn(privateId,'PRIVATE_ORCHID_794 confidential personal planning','private-first');
    const sharedId=await create({type:'team',team_id:team.id});
    assert.deepEqual((await call('/v1/agents/'+privateId)).scope,{type:'personal'});
    assert.deepEqual((await call('/v1/agents/'+sharedId)).scope,{type:'team',team_id:team.id});
    const replayHeaders={authorization:'Bearer '+token,'content-type':'application/json','idempotency-key':'company-context-immutable'};
    const replay=await fetch(new URL('/v1/agents',base),{method:'POST',headers:replayHeaders,body:JSON.stringify({scope:{type:'team',team_id:team.id}})});
    assert.equal(replay.status,201);
    const replayId=(await replay.json()).agent_id;
    const conflict=await fetch(new URL('/v1/agents',base),{method:'POST',headers:replayHeaders,body:JSON.stringify({scope:{type:'personal'}})});
    assert.equal(conflict.status,409,'replayed creation cannot change the shared partition');
    trace.push({path:'/v1/agents',method:'POST',scenario:'scope-changing replay',status:conflict.status,value:await conflict.json()});
    assert.deepEqual((await call('/v1/agents/'+replayId)).scope,{type:'team',team_id:team.id});
    await turn(sharedId,'SHARED_MAPLE_295 team launch planning','shared-first');
    const search=async(query,teamId)=>call('/v1/history/sessions/search'+(teamId?'?team_id='+teamId:''),'POST',{query});
    let found;const deadline=Date.now()+15000;
    do {found=await search('SHARED_MAPLE_295',team.id);if(found.results.length)break;await new Promise(r=>setTimeout(r,80));}while(Date.now()<deadline);
    assert.equal(found.results[0]?.session_id,sharedId,'completed real runtime turn projects into shared history');
    const lookupId=await create();
    await turn(lookupId,'LOOKUP_TEAM_MEMORY read the shared project memory','lookup-memory');
    assert.match(JSON.stringify(receipts.at(-1)),/SHARED_MAPLE_295/);
    const historyId=await create();
    await turn(historyId,'LOOKUP_TEAM_HISTORY search the shared project history','lookup-history');
    assert.match(JSON.stringify(receipts.at(-1)),/SHARED_MAPLE_295/);
    const discoverId=await create();
    await turn(discoverId,'DISCOVER_CONTEXTS list my authorized company contexts','discover-personal');
    assert.match(JSON.stringify(receipts.at(-1)),new RegExp(team.id));
    const child=await call('/v1/teams','POST',{name:'Project Alpha',company_id:team.id},201);
    const sibling=await call('/v1/teams','POST',{name:'Project Beta',company_id:team.id},201);
    const childSession=await create({type:'team',team_id:child.id});
    await turn(childSession,'LOOKUP_TEAM_MEMORY read parent company memory','parent-memory');
    assert.match(JSON.stringify(receipts.at(-1)),/SHARED_MAPLE_295/);
    teamIdentifier=sibling.id;
    await turn(childSession,'DENIED_CONTEXT LOOKUP_TEAM_MEMORY sibling content must not enter this team','sibling-memory-denied');
    assert.equal(receipts.at(-1).is_error, true, 'sibling memory access is denied at the runtime tool boundary');
    await turn(childSession,'DENIED_CONTEXT LOOKUP_TEAM_HISTORY sibling history must not enter this team','sibling-history-denied');
    await turn(childSession,'DISCOVER_CONTEXTS list only this team and parent','discover-shared');
    assert.match(JSON.stringify(receipts.at(-1)),new RegExp(child.id));
    assert.match(JSON.stringify(receipts.at(-1)),new RegExp(team.id));
    assert.doesNotMatch(JSON.stringify(receipts.at(-1)),new RegExp(sibling.id));
    teamIdentifier=team.id;
    assert.equal((await search('SHARED_MAPLE_295')).results.length,0,'team contribution does not pollute personal history');
    assert.equal((await search('PRIVATE_ORCHID_794',team.id)).results.length,0,'private contribution is absent from team history');
    token=bob;
    assert.equal((await search('SHARED_MAPLE_295',team.id)).results[0]?.session_id,sharedId);
    const shared=await call('/v1/history/sessions/'+sharedId+'/read?team_id='+team.id,'POST',{});
    assert.match(JSON.stringify(shared),/SHARED_MAPLE_295/);
    assert.deepEqual((await call('/v1/history/sessions/'+privateId+'/read?team_id='+team.id,'POST',{})).turns,[]);
    assert.equal((await search('PRIVATE_ORCHID_794')).results.length,0);
    assert.match(JSON.stringify(await call('/v1/memories/read?team_id='+team.id,'POST',{path:'MEMORY.md'})),/SHARED_MAPLE_295/);
    await call('/v1/memories/read','POST',{path:'MEMORY.md'},400);
    await call('/v1/admin/threads?operation=list&owner_id='+identity,'GET',undefined,403);
    const bobLogin=await call('/v1/credentials/claude/login','POST');
    await call('/v1/credentials/claude/login/complete','POST',{code:'managed-runtime#'+new URL(bobLogin.authorization_url).searchParams.get('state')});
    const bobSession=await create({type:'team',team_id:team.id});
    token=alice;
    await call('/v1/admin/threads?operation=list&owner_id='+bobId,'GET',undefined,403);
    await call(tp+'/members/'+bobId,'PATCH',{role:'reader'});
    token=bob;
    await search('SHARED_MAPLE_295',team.id);
    await call('/v1/agents/'+bobSession+'/turns','POST',{input:'Forbidden contribution',id:'reader-denied'},403);
    await call('/v1/memories/write?team_id='+team.id,'POST',{operation:'put',path:'MEMORY.md',content:'forbidden'},403);
    await call('/v1/agents','POST',{scope:{type:'team',team_id:team.id}},403);
    token=admin;
    await call('/v1/admin/threads?operation=list&owner_id='+identity);
    await call('/v1/history/sessions/search?team_id='+team.id,'POST',{query:'SHARED_MAPLE_295'},403);
    token=alice;await call(tp+'/members/'+bobId,'DELETE');
    token=bob;
    await call('/v1/agents/'+bobSession,'GET',undefined,403);
    await call('/v1/history/sessions/search?team_id='+team.id,'POST',{query:'SHARED_MAPLE_295'},403);
    await call('/v1/memories/read?team_id='+team.id,'POST',{path:'MEMORY.md'},403);
    await mf.dispose();await open();
    await call('/v1/history/sessions/search?team_id='+team.id,'POST',{query:'SHARED_MAPLE_295'},403);
    token=alice;
    assert.match(JSON.stringify(await call('/v1/memories/read','POST',{path:'MEMORY.md'})),/PRIVATE_ORCHID_794/);
    assert.equal((await search('SHARED_MAPLE_295',team.id)).results[0]?.session_id,sharedId);
    assert.deepEqual(errors,[]);
    console.info('COMPANY_CONTEXT_JOURNEY',{mainTurns:main.length,restarts:1,evidence});
  } finally {
    await mf?.dispose();
    await writeFile(resolve(evidence,'public-api-trace.json'),JSON.stringify(trace,null,2));
    await writeFile(resolve(evidence,'provider-trace.json'),JSON.stringify({naming,main,receipts,errors},null,2));
    await rm(persistence,{recursive:true,force:true});
  }
});
