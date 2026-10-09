// Evidence defaults to ignored output/claude-managed; NANOCODEX_CLAUDE_EVIDENCE_DIR overrides it.
// Reproduce: pnpm --filter nanocodex-managed-service run test:claude-managed
// The public Worker, account auth, SQLite Session DO, Rust Nanoclaude, private
// SessionModelEgress, broker vault and Rust OAuth state machine are production.
// Only account bootstrap (synthetic identity) and external provider HTTP are fixtures.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { mkdir, writeFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, dirname, basename } from 'node:path';
import { build } from 'esbuild';
import { builtinModules } from 'node:module';
import { Miniflare, Response as FixtureResponse, WebSocketPair } from 'miniflare';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const evidence = resolve(repo, process.env.NANOCODEX_CLAUDE_EVIDENCE_DIR ?? 'output/claude-managed');
const grantHeaders = { 'x-nanocodex-connect-user':'11111111-1111-4111-8111-111111111133', 'x-nanocodex-connect-grant-id':'0x'+'a'.repeat(64), 'x-nanocodex-connect-capabilities':JSON.stringify(['agents:read','agents:write','tools:use']), 'x-nanocodex-connect-connectors':JSON.stringify(['chatgpt']), 'x-nanocodex-connect-mcp-ids':'[]', 'content-type':'application/json' };
const identity = '11111111-1111-4111-8111-111111111133';
const bootstrap = `
import { DurableObject } from 'cloudflare:workers';
// No container is allocated; session deletion still checks legacy resources.
export class FixtureSandbox extends DurableObject {
  async clearRemoteDesktop() {}
  async destroy() {}
}
import managed, * as publicClasses from './src/index.ts';
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
      ...(capabilities?{capabilities}:{}),subjectId:'fixture:'+user,credentialId:'fixture' }, 'synthetic-claude-managed'));
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
// Every provider fixture calls capabilities through the shipped Code Mode evaluator.
const nestedCode = (name, input) => `text("NESTED_RECEIPT:"+JSON.stringify(await tools.${name}(${JSON.stringify(input)})));`;
const usedNested = block => {
  const code = block.input?.code ?? block.input?.input ?? block.input ?? '';
  return typeof code === 'string' ? [...code.matchAll(/tools\.([A-Za-z0-9_]+)\(/g)].map(match=>match[1]) : [];
};
const receiptValue = content => {
  const text = typeof content === 'string' ? content : content.map(block=>block.text??'').join('\n');
  const marker = text.match(/NESTED_RECEIPT:(.+)/);
  assert.ok(marker, text);
  const value = JSON.parse(marker[1]);
  return typeof value === 'string' && value.startsWith('{') ? JSON.parse(value) : value;
};
const assertStrict = definitions => assert.deepEqual(definitions.map(tool=>tool.name??tool.function?.description?.split('\n')[0]??tool.function?.name).sort(), ['exec','wait']);

test('Managed Code Mode Claude and mixed-family public delegation, account gates, cancellation and recovery', {timeout:240_000}, async () => {
  await mkdir(evidence,{recursive:true});
  const trace = [], upstream = [], providerErrors = [], mediaRequests = [], deniedCalls = []; let calls=0, summaries=0, writes=0, canonicalWrites=0, codexWrites=0, nestedWrites=0, allowResponses=false, sidebarCalls=0, holds=0, responsesAttempts=0, catalogOutage=false, catalogUnsupportedOnly=false, catalogRequests=0, catalogHold, hostedJourney, releaseHostedChild, mf;
  const framingRequests = {crOnly:0,truncated:0}, activeSteerRequests = [];
  let releaseActiveSteer;
  let progressAvailable = false;
  const progressRequests = [];
  const mcpTrace = [], mcpOrigins = new Set(['https://developers.openai.com','https://mcp.tempo.xyz','https://mercator.sh','https://docs.mcp.cloudflare.com','https://viem.sh','https://vocs.dev']);
  let holdMcp = false, releaseMcp;
  let compacting = false;
  let mcpHold = Promise.resolve();
  const mcpStarts = () => mcpTrace.filter(row => row.method === 'initialize').length;
  const providerImpl = async request => {
    const url = new URL(request.url);
    if (mcpOrigins.has(url.origin)) {
      if (request.method === 'GET') return new Response(null,{status:405});
      if (request.method === 'DELETE') return new Response(null,{status:204});
      const body = await request.json();
      mcpTrace.push({origin:url.origin,method:body.method,held:holdMcp});
      if (body.method === 'initialize') {
        if (holdMcp) await mcpHold;
        return Response.json({jsonrpc:'2.0',id:body.id,result:{protocolVersion:body.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'synthetic-public-mcp',version:'1'}}});
      }
      if (body.method === 'notifications/initialized') return new Response(null,{status:202});
      if (body.method === 'tools/list') return Response.json({jsonrpc:'2.0',id:body.id,result:{tools:[{name:'fixture_echo',description:'Return the synthetic MCP proof',inputSchema:{type:'object',properties:{proof:{type:'string'}},required:['proof']}}]}});
      if (body.method === 'tools/call') {
        assert.equal(body.params.name,'fixture_echo');
        return Response.json({jsonrpc:'2.0',id:body.id,result:{content:[{type:'text',text:body.params.arguments.proof}]}});
      }
      throw new Error('Unexpected MCP method '+body.method);
    }
    if (url.origin === 'https://api.openai.com' || url.origin === 'https://chatgpt.com') {
      assert.equal(url.origin,'https://api.openai.com');
      assert.equal(request.headers.get('authorization'),'Bearer sk-synthetic-openai-runtime');
      if (request.headers.get('upgrade')==='websocket') {
        const [client,server]=Object.values(new WebSocketPair()); server.accept();
        server.addEventListener('close',()=>server.close(1000));
        server.addEventListener('message',event=>{
          try {
            const body=JSON.parse(event.data);
            assert.equal(body.model,'gpt-6.1-sol');
            assert.match(JSON.stringify(body.input),/GPT_MCP_DISCOVERY_PROBE/);
            assertStrict([...(body.tools??[]),...(body.input??[]).filter(item=>item.type==='additional_tools').flatMap(item=>item.tools)]);
            upstream.push({provider:'openai',model:body.model,scenario:'GPT_MCP_DISCOVERY_PROBE',transport:'websocket'});
            server.send(JSON.stringify({type:'response.created',response:{id:'gpt-mcp-probe',status:'in_progress'}}));
            server.send(JSON.stringify({type:'response.output_text.delta',output_index:0,delta:'CLAUDE_TOOL_DONE_GPT_MCP_PROBE'}));
            server.send(JSON.stringify({type:'response.completed',response:{id:'gpt-mcp-probe',status:'completed',end_turn:true,output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'CLAUDE_TOOL_DONE_GPT_MCP_PROBE'}]}],usage:{input_tokens:10,output_tokens:2,total_tokens:12}}}));
          } catch(error) { providerErrors.push({scenario:'GPT_MCP_DISCOVERY_PROBE',error:String(error)});server.close(1011,'invalid fixture request'); }
        });
        return new FixtureResponse(null,{status:101,webSocket:client});
      }
      const body=await request.json(), encoded=JSON.stringify(body.input);
      if (encoded.includes('GPT_MCP_DISCOVERY_PROBE') && body.model==='gpt-6.1-sol') {
        assertStrict([...(body.tools??[]),...(body.input??[]).filter(item=>item.type==='additional_tools').flatMap(item=>item.tools)]);
        upstream.push({provider:'openai',model:body.model,scenario:'GPT_MCP_DISCOVERY_PROBE'});
        return new Response(`data: ${JSON.stringify({type:'response.completed',response:{id:'gpt-mcp-probe',status:'completed',output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'CLAUDE_TOOL_DONE_GPT_MCP_PROBE'}]}],usage:{input_tokens:10,output_tokens:2,total_tokens:12}}})}\n\n`,{headers:{'content-type':'text/event-stream'}});
      }
      if (body.model==='gpt-6-luna') {
        assert.match(body.instructions,/Write a short session title/);
        assert.match(encoded,/Delegate mixed Claude child|Try disconnected mixed child|Delegate nested gateway grandchild|GPT_MCP_DISCOVERY_PROBE|MCP_LAZY_|HOSTED_IDLE_/,'only the Codex gateway root requests a sidebar title');
        sidebarCalls++;
        return Response.json({id:'synthetic-title',output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'Verify native Claude delegation'}]}],usage:{input_tokens:2,output_tokens:2,total_tokens:4}});
      }
      responsesAttempts++;
      let output;
      if (hostedJourney && encoded.includes(hostedJourney.marker)) {
        if (encoded.includes(hostedJourney.childTask)) {
          hostedJourney.childCalls++;
          if (hostedJourney.childCalls===1) {
            hostedJourney.childEntered();
            await hostedJourney.childGate;
          }
          const submitted=body.input.some(item=>item.type==='custom_tool_call'&&item.input.includes('tools.submit_result('));
          if (!submitted) output=[{type:'custom_tool_call',call_id:'hosted-child-submit-'+responsesAttempts,name:'exec',input:nestedCode('submit_result',{output:hostedJourney.childOutput})}];
          else output=[{type:'message',role:'assistant',content:[{type:'output_text',text:'Hosted child finished'}]}];
          return new Response(`data: ${JSON.stringify({type:'response.completed',response:{id:'hosted-child-'+responsesAttempts,status:'completed',output,usage:{input_tokens:10,output_tokens:2,total_tokens:12}}})}\n\n`,{headers:{'content-type':'text/event-stream'}});
        }
      }
      assert.equal(allowResponses,true,'only an explicitly selected Codex child may use Responses');
      assert.equal(body.model,'gpt-6.1-sol');
      assert.match(encoded,/CANONICAL_CODEX_PROOF/);
      assertStrict([...(body.tools??[]),...(body.input??[]).filter(item=>item.type==='additional_tools').flatMap(item=>item.tools)]);
      upstream.push({provider:'openai',model:body.model,body});
      if(!encoded.includes('CODEX_EFFECT_ACK')) {
        codexWrites++;
        output=[{type:'custom_tool_call',call_id:'codex-effect-'+responsesAttempts,name:'exec',input:'const effect = await tools.exec_command({cmd:"printf CODEX_DURABLE_CHILD_PROOF > /brain/codex-child.txt",workdir:"/brain"}); if (effect.exit_code !== 0) throw new Error(JSON.stringify(effect)); text("CODEX_EFFECT_ACK");'}];
      } else if(!body.input.some(item=>item.type==='custom_tool_call'&&item.input.includes('tools.submit_result('))) {
        output=[{type:'custom_tool_call',call_id:'codex-submit-'+responsesAttempts,name:'exec',input:nestedCode('submit_result',{output:'CODEX_DURABLE_CHILD_PROOF'})}];
      } else output=[{type:'message',role:'assistant',content:[{type:'output_text',text:'Codex child finished'}]}];
      return new Response(`data: ${JSON.stringify({type:'response.completed',response:{id:'codex-response-'+responsesAttempts,status:'completed',output,usage:{input_tokens:10,output_tokens:2,total_tokens:12}}})}\n\n`,{headers:{'content-type':'text/event-stream'}});
    }
    if (url.origin === 'https://openrouter.ai' && url.pathname === '/api/v1/chat/completions') {
      assert.equal(request.headers.get('authorization'),'Bearer synthetic-gateway-key');
      const body=await request.json();
      upstream.push({provider:'openrouter',model:body.model,messages:body.messages,tools:body.tools});
      assertStrict(body.tools);
      const tool = name => {
        const definition=body.tools.find(row=>row.function.name===name || row.function.name.endsWith('_'+name) || row.function.description.startsWith(name+'\n'));
        assert.ok(definition,`canonical ${name} is declared`); return definition.function.name;
      };
      const reply=(message,finish_reason)=>new Response([
        {choices:[{index:0,delta:{...message,...(message.tool_calls?{tool_calls:message.tool_calls.map((call,index)=>({...call,index}))}:{})},finish_reason:null}]},
        {choices:[{index:0,delta:{},finish_reason}]},'[DONE]',
      ].map(value=>`data: ${typeof value==='string'?value:JSON.stringify(value)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
      const use=(name,input)=>reply({tool_calls:[{id:'mixed-'+crypto.randomUUID(),type:'function',function:{name:tool('exec'),arguments:JSON.stringify({input:name==='exec'?input.input:nestedCode(name,input)})}}]},'tool_calls');
      const latest=body.messages.at(-1);
      // Inspect what the real managed Worker + WASM sends to the provider.
      // The fixture cannot establish an LLM's natural-language compliance, but
      // it must reject a missing identity or one inherited from a parent route.
      const instructions=body.messages.filter(message=>['system','developer'].includes(message.role))
        .map(message=>typeof message.content==='string'?message.content:message.content.map(part=>part.text??'').join('')).join('\n');
      const identities=[...instructions.matchAll(/<runtime_model_identity>\nmodel_id: ([^\n]+)\n/g)].map(match=>match[1]);
      assert.deepEqual(identities,[body.model.split('/').at(-1)],'runtime identity agrees with the actual gateway model');
      if (hostedJourney && body.model.split('/').at(-1)===hostedJourney.rootModel && JSON.stringify(body.messages).includes(hostedJourney.marker)) {
        hostedJourney.rootRequests++;
        if (hostedJourney.rootInitialCompleted) {
          hostedJourney.continuationRequests++;
          if (hostedJourney.continuationRequests===1) return use('list_agents',{include_completed:true});
          assert.equal(receiptValue(latest.content).agents[0].status.output,hostedJourney.childOutput);
          return reply({content:hostedJourney.rootOutput},'stop');
        }
        if (latest.role!=='tool') return use('spawn_agent',{role:'hosted completion child',task:hostedJourney.childTask,harness:hostedJourney.childHarness,model:hostedJourney.childModel,thinking:'low',output_contract:{kind:'string'}});
        hostedJourney.rootInitialCompleted=true;
        return reply({content:'CLAUDE_TOOL_DONE_HOSTED_CHILD_DELEGATED'},'stop');
      }
      if (JSON.stringify(body.messages).includes('MODEL_IDENTITY_PROBE')) {
        assert.equal(body.model,'xiaomi/mimo-v2.6-pro');
        if (JSON.stringify(latest.content).includes('arent u mimo')) {
          assert.ok(body.messages.some(message=>message.role==='assistant'&&message.content==='CLAUDE_TOOL_DONE_MODEL_IDENTITY: mimo-v2.6-pro'),
            'the second question restores the first answer after a Worker restart');
        }
        trace.push({scenario:'managed model identity',model:body.model,identity:identities[0],question:latest.content});
        return reply({content:'CLAUDE_TOOL_DONE_MODEL_IDENTITY: '+identities[0]},'stop');
      }
      if (body.model==='xiaomi/mimo-v2.6-pro') {
        assert.match(JSON.stringify(body.messages),/GATEWAY_GRANDCHILD_TASK/);
        const used=body.messages.flatMap(message=>message.tool_calls??[]).flatMap(call=>usedNested({input:JSON.parse(call.function.arguments)}));
        if (!used.includes('exec_command')) {
          nestedWrites++;
          return use('exec',{input:'const effect = await tools.exec_command({cmd:"printf GATEWAY_GRANDCHILD_DURABLE_PROOF > /brain/gateway-grandchild.txt && cat /brain/gateway-grandchild.txt",workdir:"/brain"}); if (effect.exit_code !== 0) throw new Error(JSON.stringify(effect)); text(effect.output);'});
        }
        if (!used.includes('submit_result')) {
          assert.match(latest.content,/GATEWAY_GRANDCHILD_DURABLE_PROOF/,'gateway grandchild receives its real filesystem effect result');
          return use('submit_result',{output:'GATEWAY_GRANDCHILD_DURABLE_PROOF'});
        }
        return reply({content:'gateway grandchild finished'},'stop');
      }
      if (JSON.stringify(body.messages).includes('Delegate nested gateway grandchild')) {
        assert.equal(body.model,'moonshotai/kimi-k3','root retains its gateway model');
        if (latest.role!=='tool') return use('spawn_agent',{role:'nested Claude specialist',task:'NESTED_CLAUDE_PARENT: delegate a gateway Codex grandchild and return its proof',harness:'claude',model:'claude-opus-4-6',thinking:'low',output_contract:{kind:'string'}});
        const decoded=receiptValue(latest.content);
        if (decoded.agent_id!==undefined) return use('wait_agent',{agent_ids:[decoded.agent_id],timeout_ms:10000});
        assert.equal(decoded.agents[0].status.state,'completed',latest.content);
        assert.equal(decoded.agents[0].status.output,'GATEWAY_GRANDCHILD_DURABLE_PROOF');
        return reply({content:'CLAUDE_TOOL_DONE_NESTED_GATEWAY: '+decoded.agents[0].status.output},'stop');
      }
      if(latest.role!=='tool') return use('spawn_agent',{role:'mixed proof specialist',task:'CANONICAL_CHILD_PROOF: write proof then submit result',harness:'claude',model:'claude-opus-4-6',thinking:'low',output_contract:{kind:'string'}});
      if(JSON.stringify(body.messages).includes('Try disconnected mixed child')) {
        assert.match(latest.content,/failed|unavailable|authorized/i);
        return reply({content:'CLAUDE_TOOL_DONE_DISCONNECTED_CHILD'},'stop');
      }
      const decoded=receiptValue(latest.content);
      if(decoded.agent_id!==undefined) return use('wait_agent',{agent_ids:[decoded.agent_id],timeout_ms:10000});
      assert.equal(decoded.agents[0].status.state,'completed',latest.content);
      assert.equal(decoded.agents[0].status.output,'CANONICAL_DURABLE_CHILD_PROOF');
      return reply({content:'CLAUDE_TOOL_DONE_MIXED_CHILD'},'stop');
    }
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/models') {
      catalogRequests++;
      if (catalogHold) await catalogHold;
      if(catalogOutage)return new Response('synthetic catalog unavailable',{status:503});
      assert.match(request.headers.get('authorization')??'',/^Bearer synthetic-claude-(?:managed-runtime|profile-uncertain)/);
      if(catalogUnsupportedOnly)return Response.json({data:[{id:'claude-gated-unverified',display_name:'Not supported'}],has_more:false});
      assert.equal(url.searchParams.get('limit'),'100');
      if (!url.searchParams.has('after_id')) return Response.json({data:[{id:'claude-gated-unverified',display_name:'Not supported'}],has_more:true,last_id:'claude-gated-unverified'});
      assert.equal(url.searchParams.get('after_id'),'claude-gated-unverified');
      return Response.json({data:[{id:'claude-sonnet-4-6',display_name:'Claude Sonnet 4.6'},
        {id:'claude-opus-4-6',display_name:'Claude Opus 4.6'}, ...(progressAvailable?[{id:'claude-opus-5-5',display_name:'Claude Opus 5.5'}]:[]), {id:'claude-gated-unverified',display_name:'Not supported'}],has_more:false});
    }
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/messages') {
      assert.match(request.headers.get('authorization')??'',/^Bearer synthetic-claude-managed-runtime/);
      for (const name of ['x-nanocodex-subject','x-nanocodex-session-model-owner','x-nanocodex-claude-host','x-api-key']) assert.equal(request.headers.has(name),false,`private ${name} stripped`);
      assert.equal(url.search,'?beta=true');
      assert.equal(request.headers.get('x-app'),'cli');
      assert.equal(request.headers.has('x-claude-code-request-class'),false);
      assert.equal(request.headers.get('anthropic-dangerous-direct-browser-access'),'true');
      assert.equal(request.headers.get('user-agent'),'claude-cli/2.1.280 (external, cli)');
      assert.equal(request.headers.get('x-stainless-runtime'),'node');
      assert.equal(request.headers.get('x-stainless-lang'),'js');
      assert.equal(request.headers.get('x-stainless-package-version'),'0.112.1');
      const wire = await request.text(); const body = JSON.parse(wire); calls++;
      if (body.model === 'claude-opus-5-5') {
        assert.deepEqual(body.thinking,{type:'adaptive',display:'updates'});
        assert.equal(body.output_config.effort,'high');
        assert.equal((request.headers.get('anthropic-beta')??'').split(',').filter(beta=>beta==='thinking-display-updates-2026-08-18').length,1);
        progressRequests.push(body);
        if (progressRequests.length > 1) {
          const blocks=body.messages.flatMap(message=>Array.isArray(message.content)?message.content:[]);
          assert.ok(blocks.some(block=>block.signature==='PRIVATE_PROGRESS_SIGNATURE'&&block.thinking==='Checking the saved record.'));
          assert.ok(blocks.some(block=>block.signature==='PRIVATE_REASONING_SIGNATURE'&&block.thinking===''));
          return sse({type:'text',text:'CLAUDE_TOOL_DONE_PROGRESS'},'end_turn','progress-final');
        }
        const events=[
          {type:'message_start',message:{id:'progress-live',role:'assistant',model:body.model,content:[],usage:{input_tokens:10,output_tokens:0}}},
          {type:'content_block_start',index:0,content_block:{type:'thinking',thinking:'',signature:''}},
          {type:'content_block_delta',index:0,delta:{type:'thinking_delta',thinking:''}},
          {type:'content_block_delta',index:0,delta:{type:'signature_delta',signature:'PRIVATE_REASONING_SIGNATURE'}},
          {type:'content_block_stop',index:0},
          {type:'content_block_start',index:1,content_block:{type:'thinking',thinking:'',signature:''}},
          {type:'content_block_delta',index:1,delta:{type:'thinking_delta',thinking:'Checking the '}},
          {type:'content_block_delta',index:1,delta:{type:'thinking_delta',thinking:'saved record.'}},
          {type:'content_block_delta',index:1,delta:{type:'signature_delta',signature:'PRIVATE_PROGRESS_SIGNATURE'}},
          {type:'content_block_stop',index:1},
          {type:'content_block_start',index:2,content_block:{type:'tool_use',id:'progress-tool',name:'_exec',input:{}}},
          {type:'content_block_delta',index:2,delta:{type:'input_json_delta',partial_json:JSON.stringify({code:'text("record verified");'})}},
          {type:'content_block_stop',index:2},
          {type:'message_delta',delta:{stop_reason:'tool_use'},usage:{output_tokens:10}},
          {type:'message_stop'},
        ];
        return new Response(events.map(event=>`data: ${JSON.stringify(event)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
      }
      assert.ok(['claude-sonnet-4-6','claude-opus-4-6'].includes(body.model)); assert.equal(body.stream,true);
      assert.match(body.system[0].text,/^x-anthropic-billing-header: cc_version=2\.1\.280\.[0-9a-f]{3}; cc_entrypoint=cli; cch=[0-9a-f]{5};$/);
      assert.equal(body.system[1].text,"You are Claude Code, Anthropic's official CLI for Claude.");
      // Claude Code cache shape for roots and children: identity + instructions
      // system markers and one moving final-block marker, all 1h, never top-level.
      const oneHour={type:'ephemeral',ttl:'1h'};
      assert.equal(body.cache_control,undefined,'no top-level automatic cache on the subscription wire');
      assert.deepEqual(body.system.map(block=>block.cache_control??null),[null,oneHour,oneHour]);
      const markedBlocks=body.messages.flatMap(message=>message.content.filter(block=>block.cache_control!==undefined));
      const cacheTail=[...body.messages.at(-1).content].reverse().find(block=>!['thinking','redacted_thinking'].includes(block.type)&&!(block.type==='text'&&!block.text));
      assert.deepEqual(markedBlocks,[cacheTail],'exactly one message marker, on the final cacheable block');
      assert.deepEqual(cacheTail.cache_control,oneHour);
      assert.equal(JSON.parse(body.metadata.user_id).session_id,request.headers.get('x-claude-code-session-id'));
      assert.equal(request.headers.get('accept'),'application/json');
      assert.equal(body.output_config.effort,'low');
      const wireNames=(body.tools??[]).map(t=>t.name);
      const names=wireNames.map(name=>name.replace(/^_/,''));
      assert.deepEqual(names.sort(),['exec','wait'],'Claude roots and children expose only Code Mode');
      assert.deepEqual(wireNames,['_exec','_wait']);
      const catalog=body.tools.find(tool=>tool.name.replace(/^_/,'')==='exec').description;
      const admits=name=>catalog.includes('"name":"'+name+'"');
      for(const legacy of ['Task','TaskOutput','TaskStop','Bash','BashOutput'])assert.equal(admits(legacy),false,'legacy capabilities are absent');
      upstream.push({wire,request:calls,model:body.model,tool_names:names,wire_tool_names:wireNames,message_count:body.messages.length,
        tool_uses:body.messages.flatMap(message=>Array.isArray(message.content)?message.content.filter(block=>block.type==='tool_use').map(block=>block.name):[]),
        tool_result_count:body.messages.flatMap(message=>Array.isArray(message.content)?message.content.filter(block=>block.type==='tool_result'):[]).length,
        prior_proof_present:JSON.stringify(body.messages).includes('NATIVE_CLAUDE_DURABLE_PROOF'),summary_present:JSON.stringify(body.messages).includes('NATIVE_SUMMARY'),effort:body.output_config.effort});
      const latest=body.messages.at(-1), result=Array.isArray(latest.content)&&latest.content.find(b=>b.type==='tool_result');
      const encodedHistory = JSON.stringify(body.messages);
      if (encodedHistory.includes('MANAGED_FRAME_CR_ONLY')) {
        framingRequests.crOnly++;
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_CR_ONLY'},'end_turn',`message-${calls}`,'\r');
      }
      if (encodedHistory.includes('MANAGED_FRAME_TRUNCATED')) {
        framingRequests.truncated++;
        return sse({type:'text',text:'MUST_NOT_COMPLETE_TRUNCATED'},'end_turn',`message-${calls}`,'\r',false);
      }
      if (encodedHistory.includes('MANAGED_ACTIVE_STEER')) {
        activeSteerRequests.push(body.messages);
        if (activeSteerRequests.length === 1) {
          await new Promise(resolve=>{releaseActiveSteer=resolve;});
          return sse({type:'text',text:'Initial answer before queued steer'},'end_turn',`message-${calls}`);
        }
        assert.equal(activeSteerRequests.length,2,'steering must not replay the initial model request');
        const text=JSON.stringify(body.messages);
        assert.ok(text.includes('first managed steering correction') && text.includes('second managed é correction'),text);
        assert.ok(text.indexOf('first managed steering correction') < text.indexOf('second managed é correction'),'queued steering preserves order');
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_ACTIVE_STEER'},'end_turn',`message-${calls}`);
      }
      const canonicalTask = encodedHistory.includes('CANONICAL_CHILD_PROOF');
      const codexRoot = encodedHistory.includes('Delegate canonical Codex child');
      const canonicalRoot = encodedHistory.includes('Delegate canonical Claude child') || codexRoot;
      const unavailableChild = encodedHistory.includes('Try unavailable canonical child');
      const disabledChild = encodedHistory.includes('Try disabled canonical child');
      const code = source => sse({type:'tool_use',id:`exec-${calls}`,name:'exec',input:{code:source}},'tool_use',`message-${calls}`);
      const use = (name, input) => code(nestedCode(name,input));
      if (encodedHistory.includes('CANONICAL_COMMAND_PROBE')) {
        assert.ok(admits('exec_command') && admits('write_stdin'));
        if (!result) return code(`
          const result = await tools.exec_command({cmd:"printf CANONICAL_COMMAND_OK",yield_time_ms:1000,max_output_tokens:1000});
          if (result.exit_code !== 0 || result.output !== "CANONICAL_COMMAND_OK") throw new Error(JSON.stringify(result));
          let stale = false, staleReceipt;
          try { staleReceipt = await tools.write_stdin({session_id:987654321,chars:"",yield_time_ms:1000,max_output_tokens:1000}); stale = /unknown or stale/.test(JSON.stringify(staleReceipt)); }
          catch (error) { staleReceipt = String(error); stale = /unknown or stale/.test(staleReceipt); }
          if (!stale) throw new Error("unexpected stale receipt: " + JSON.stringify(staleReceipt));
          for (const name of ["Bash", "BashOutput"]) {
            let unavailable = false;
            try { await tools[name]({command:"printf MUST_NOT_RUN"}); }
            catch (error) { unavailable = /TOOL_NOT_AVAILABLE|not a function|not available|unavailable|unknown tool/.test(String(error)); }
            if (!unavailable) throw new Error(name + " remains callable");
          }
          text("CANONICAL_COMMAND_RECEIPT");
        `);
        assert.equal(result.is_error ?? false, false, JSON.stringify(result));
        assert.match(JSON.stringify(result.content), /CANONICAL_COMMAND_RECEIPT/);
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_CANONICAL_COMMAND'},'end_turn',`message-${calls}`);
      }
      if (hostedJourney && encodedHistory.includes(hostedJourney.marker)
        && (body.model===hostedJourney.rootModel || (hostedJourney.childHarness==='claude' && body.model===hostedJourney.childModel))) {
        if (hostedJourney.childHarness==='claude' && body.model===hostedJourney.childModel && body.model!==hostedJourney.rootModel) {
          hostedJourney.childCalls++;
          if (hostedJourney.childCalls===1) {
            hostedJourney.childEntered();
            await hostedJourney.childGate;
          }
          if (!encodedHistory.includes('tools.submit_result(')) return use('submit_result',{output:hostedJourney.childOutput});
          return sse({type:'text',text:'Hosted child finished'},'end_turn',`message-${calls}`);
        }
        hostedJourney.rootRequests++;
        if (hostedJourney.rootInitialCompleted) {
          hostedJourney.continuationRequests++;
          if (hostedJourney.continuationRequests===1) return use('list_agents',{include_completed:true});
          assert.equal(receiptValue(result.content).agents[0].status.output,hostedJourney.childOutput);
          return sse({type:'text',text:hostedJourney.rootOutput},'end_turn',`message-${calls}`);
        }
        if (!encodedHistory.includes('tools.spawn_agent(')) return use('spawn_agent',{role:'hosted completion child',task:hostedJourney.childTask,harness:hostedJourney.childHarness,model:hostedJourney.childModel,thinking:'low',output_contract:{kind:'string'}});
        if (hostedJourney.sourceOutcome==='cancelled') {
          hostedJourney.rootHeld=true;
          return new Response(new ReadableStream({start(controller){controller.enqueue(new TextEncoder().encode('event: message_start\ndata: '+JSON.stringify({type:'message_start',message:{id:'hosted-cancel',role:'assistant',model:body.model,content:[],usage:{input_tokens:10,output_tokens:0}}})+'\n\n'));}}),{headers:{'content-type':'text/event-stream'}});
        }
        hostedJourney.rootInitialCompleted=true;
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_HOSTED_CHILD_DELEGATED'},'end_turn',`message-${calls}`);
      }
      const uses = body.messages.flatMap(message=>Array.isArray(message.content)?message.content.filter(block=>block.type==='tool_use').flatMap(usedNested):[]);
      const resultText = result ? (typeof result.content==='string'?result.content:result.content.map(block=>block.text??'').join('\n')) : '';
      const running = resultText.match(/Script running with cell ID ([^\s]+)/)?.[1];
      if (running) return sse({type:'tool_use',id:`wait-${calls}`,name:'wait',input:{cell_id:running,yield_time_ms:10000}},'tool_use',`message-${calls}`);
      const deniedName = encodedHistory.includes('Try forbidden Read') ? 'Read'
        : encodedHistory.includes('Try forbidden Write') ? 'Write' : disabledChild ? 'spawn_agent' : undefined;
      if (result && deniedName) {
        assert.equal(admits(deniedName),false,'denied capability remains outside the nested catalog');
        assert.match(resultText,/error|unavailable|not a function|not defined|unknown|not found/i,'unadmitted call returns an error');
        deniedCalls.push(deniedName);
        return sse({type:'text',text:`CLAUDE_TOOL_DONE_DENIED_${calls}`},'end_turn',`message-${calls}`);
      }
      if (encodedHistory.includes('SHARED_PLATFORM_NATIVE_PROBE')) {
        for (const name of ['environment','memories__write','memories__read','find_session','read_session','mcp__cua_repl__js','mcp__cua_repl__js_reset'])
          assert.ok(admits(name),`Claude retains shared platform tool ${name}`);
        // Keep the cell alive across an explicit yield so wait is exercised deterministically.
        if (!uses.length) return code('// @exec: {"yield_time_ms": 0}\n'+'yield_control(); await new Promise(resolve=>setTimeout(resolve,100)); text(ALL_TOOLS.map(tool=>tool.name)); '+nestedCode('environment',{}));
        assert.equal(result?.is_error??false,false,JSON.stringify(result));
        if (uses.at(-1)==='environment') {
          assert.match(JSON.stringify(result),/cloudflare-durable-object/);
          assert.match(encodedHistory,/spawn_agent/);assert.match(encodedHistory,/wait_agent/);
          return use('memories__write',{operation:'put',path:'memory/2026-01-01-claude-platform-proof.md',content:'SHARED_PLATFORM_MEMORY_PROOF'});
        }
        if (uses.at(-1)==='memories__write') return use('memories__read',{path:'memory/2026-01-01-claude-platform-proof.md'});
        if (uses.at(-1)==='memories__read') {
          assert.match(JSON.stringify(result),/SHARED_PLATFORM_MEMORY_PROOF/);
          return code('try { text(await tools.mcp__cua_repl__js({workdir:"/synthetic-missing-hand",code:"await cua.getState()"})); } catch (error) { text(String(error)); }');
        }
        assert.match(resultText,/hand|machine|namespace|workdir|not attached/i,'CUA returns an actionable missing-Hand error through exec');
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_SHARED_PLATFORM'},'end_turn',`message-${calls}`);
      }
      if (encodedHistory.includes('MCP_LAZY_')) {
        if (!uses.length) {
          assert.ok(admits('MCPToolSearch') && admits('MCPExecute'),'fixed MCP schemas are available before discovery');
          return encodedHistory.includes('MCP_LAZY_DIRECT')
            ? use('MCPExecute',{name:'mcp__openaiDeveloperDocs__fixture_echo',arguments:{proof:'MCP_LAZY_PUBLIC_PROOF'}})
            : use('MCPToolSearch',{query:'fixture_echo',limit:8});
        }
        assert.equal(result?.is_error??false,false,JSON.stringify(result));
        if (uses.at(-1)==='MCPToolSearch') {
          const discovered=receiptValue(result.content);
          assert.ok(discovered.some(namespace=>namespace.name==='mcp__openaiDeveloperDocs__'&&namespace.tools?.some(tool=>tool.name==='fixture_echo')),'MCP search returns the exact callable namespace and tool');
          return use('MCPExecute',{name:'mcp__openaiDeveloperDocs__fixture_echo',arguments:{proof:'MCP_LAZY_PUBLIC_PROOF'}});
        }
        assert.match(JSON.stringify(result),/MCP_LAZY_PUBLIC_PROOF/);
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_MCP_LAZY'},'end_turn',`message-${calls}`);
      }
      if (encodedHistory.includes('NESTED_CLAUDE_PARENT')) {
        assert.equal(body.model,'claude-opus-4-6','middle generation uses the native Claude provider');
        const toolsUsed=uses;
        if (!toolsUsed.length) return use('spawn_agent',{role:'gateway proof specialist',task:'GATEWAY_GRANDCHILD_TASK: write proof, read it back, then submit result',harness:'codex',model:'mimo',thinking:'low',output_contract:{kind:'string'}});
        if (toolsUsed.includes('submit_result')) return sse({type:'text',text:'nested Claude parent finished'},'end_turn',`message-${calls}`);
        assert.equal(result?.is_error??false,false,JSON.stringify(result));
        const decoded=receiptValue(result.content);
        if (toolsUsed.at(-1)==='spawn_agent') return use('wait_agent',{agent_ids:[decoded.agent_id],timeout_ms:10000});
        assert.equal(decoded.agents[0].status.state,'completed',JSON.stringify(decoded));
        assert.equal(decoded.agents[0].status.output,'GATEWAY_GRANDCHILD_DURABLE_PROOF');
        return use('submit_result',{output:decoded.agents[0].status.output});
      }
      if (canonicalTask && !canonicalRoot && !unavailableChild && !disabledChild) {
        assert.equal(body.model,'claude-opus-4-6','explicit child model is routed separately from the root');
        const toolsUsed=uses;
        if (!toolsUsed.includes('Write')) { canonicalWrites++; return use('Write',{file_path:'/brain/canonical-child.txt',content:'CANONICAL_DURABLE_CHILD_PROOF'}); }
        if (!toolsUsed.includes('submit_result')) {
          assert.equal(result?.is_error??false,false,'child Write retains managed execution authority');
          return use('submit_result',{output:'CANONICAL_DURABLE_CHILD_PROOF'});
        }
        return sse({type:'text',text:'canonical child finished'},'end_turn',`message-${calls}`);
      }
      if (canonicalRoot || unavailableChild || disabledChild) {
        const toolUses=uses;
        if (!toolUses.length) {
          return use('spawn_agent',{role:'proof specialist',task:codexRoot?'CANONICAL_CODEX_PROOF: write proof then submit result':'CANONICAL_CHILD_PROOF: write proof then submit result',harness:codexRoot?'codex':'claude',model:codexRoot?'gpt-6.1-sol':unavailableChild?'claude-sonnet-5-5':'claude-opus-4-6',thinking:'low',output_contract:{kind:'string'}});
        }
        if (unavailableChild) {
          assert.match(JSON.stringify(result),/error|failed|unavailable/i);
          return sse({type:'text',text:'CLAUDE_TOOL_DONE_UNAVAILABLE_CHILD'},'end_turn',`message-${calls}`);
        }
        const content = typeof result.content === 'string' ? result.content : JSON.stringify(result.content);
        const decoded = receiptValue(result.content);
        if (toolUses.at(-1)==='spawn_agent') return use('wait_agent',{agent_ids:[decoded.agent_id],timeout_ms:10000});
        assert.equal(decoded.agents[0].status.state,'completed',content);
        assert.equal(decoded.agents[0].status.output,codexRoot?'CODEX_DURABLE_CHILD_PROOF':'CANONICAL_DURABLE_CHILD_PROOF');
        return sse({type:'text',text:'CLAUDE_TOOL_DONE_CANONICAL_CHILD'},'end_turn',`message-${calls}`);
      }
      if (encodedHistory.includes('MULTIMODAL_PROOF')) {
        mediaRequests.push({model:body.model,latest:body.messages.at(-1),messages:body.messages});
        // Stream real text deltas so the transcript identity check is not vacuous.
        const id=`message-${calls}`, events=[
          {type:'message_start',message:{id,role:'assistant',model:body.model,content:[],usage:{input_tokens:10,output_tokens:0}}},
          {type:'content_block_start',index:0,content_block:{type:'text',text:''}},
          {type:'content_block_delta',index:0,delta:{type:'text_delta',text:'CLAUDE_TOOL_DONE_'}},
          {type:'content_block_delta',index:0,delta:{type:'text_delta',text:`MEDIA_${calls}`}},
          {type:'content_block_stop',index:0},
          {type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage:{output_tokens:2}},
          {type:'message_stop'},
        ];
        return new Response(events.map(e=>`event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
      }
      if(result) {
        assert.equal(result.is_error??false,false,'admitted native tools succeed');
        return sse({type:'text',text:`CLAUDE_TOOL_DONE_${calls}`},'end_turn',`message-${calls}`);
      }
      const prompt = JSON.stringify(latest.content);
      if(compacting) {
        assert.equal(body.tool_choice?.type,'none','compaction keeps tools disabled');
        summaries++; return sse({type:'text',text:'NATIVE_SUMMARY durable proof already written; never repeat Write'},'end_turn',`summary-${calls}`);
      }
      if(prompt.includes('Try forbidden Read')) {
        assert.ok(admits('Write') && !admits('Read'),'requested allowlist applies inside exec');
        return use('Read',{file_path:'/brain/proof.txt'});
      }
      if(prompt.includes('Try forbidden Write')) return use('Write',{file_path:'/brain/denied.txt',content:'MUST_NOT_EXIST'});
      if(prompt.includes('Hold until cancelled')) {
        holds++; return new Response(new ReadableStream({ start(controller) {
          controller.enqueue(new TextEncoder().encode(`event: message_start\ndata: ${JSON.stringify({type:'message_start',message:{id:'cancel-fixture',role:'assistant',model:body.model,content:[],usage:{input_tokens:10,output_tokens:0}}})}\n\n`));
        } }),{headers:{'content-type':'text/event-stream'}});
      }
      assert.ok(admits('exec_command'));assert.ok(admits('Write'));assert.ok(admits('Read'));
      if(prompt.includes('Write durable proof')){writes++;return use('Write',{file_path:'/brain/proof.txt',content:'NATIVE_CLAUDE_DURABLE_PROOF'});}
      if(prompt.includes('Read durable proof')){
        assert.ok(JSON.stringify(body.messages).includes(summaries?'NATIVE_SUMMARY':'NATIVE_CLAUDE_DURABLE_PROOF'),'prior native history/summary persisted');
        return use('Read',{file_path:'/brain/proof.txt'});
      }
      return use('exec_command',{cmd:prompt.includes('Check denied file')?'test ! -e /brain/denied.txt && echo NO_UNAUTHORIZED_FILE':'cat /brain/proof.txt',workdir:'/brain',yield_time_ms:1000,max_output_tokens:1000});
    }
    const response = await claudeProvider(request); if(response)return response;
    return new Response('Unexpected external fixture request '+url.origin+url.pathname,{status:502});
  };
  const provider=async request=>{
    try{return await providerImpl(request);}
    catch(error){providerErrors.push({url:request.url,error:String(error)});console.error('CLAUDE_FIXTURE_FAILURE',String(error));throw error;}
  };
  const managedModules=await bundle(bootstrap,resolve(repo,'js/managed'),'managed-journey');
  const egressModules=await bundle(`export * from './src/egress.ts'; export { default } from './src/egress.ts';`,resolve(repo,'js/egress'),'egress-journey');
  const persistence=resolve(evidence,'sqlite-'+crypto.randomUUID());
  const options={durableObjectsPersist:persistence,r2Persist:resolve(persistence,'r2'),workers:[
    {name:'managed',modulesRoot:'/',modules:managedModules,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],
      bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},
      serviceBindings:{NANOCODEX:'egress',NANOCODEX_SESSION_MODEL_EGRESS:{name:'egress',entrypoint:'SessionModelEgress'}},
      durableObjects:Object.fromEntries([['NANOCODEX_AUTH','NonceStorage'],['NANOCODEX_USERS','UserAccount'],['NANOCODEX_ORGANIZATIONS','Organization'],['NANOCODEX_API_KEYS','ApiKeyRecord'],['NANOCODEX_SESSIONS','DurableAgentSession'],['NANOCODEX_ACCOUNT_TOOLS','AccountHostedTools'],['NANOCODEX_VM_HOST_POOLS','VmHostPool'],['NANOCODEX_MEMORY','MemoryScope'],['NANOCODEX_SANDBOXES','FixtureSandbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),
      r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],outboundService:provider},
    {name:'egress',modulesRoot:'/',modules:egressModules,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],
      bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},
      serviceBindings:{MANAGED_AGENT_OWNERSHIP:{name:'managed',entrypoint:'ManagedAgentOwnership'}},
      durableObjects:Object.fromEntries([['USER_CREDENTIALS','UserCredentialBroker'],['AGENT_SUBJECTS','AgentSubjectDirectory'],['USER_CONNECTORS','UserConnectorBroker'],['MCP_CONNECTIONS','McpConnectionDirectory'],['SPOTIFY_RATE_LIMITS','SpotifyRateLimit'],['GMAIL_PUSH_MAILBOXES','GmailPushMailbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),outboundService:provider},
  ]};
  let token;
  const call=async(path,method='GET',body,status=200,headers={})=>{
    const response=await mf.dispatchFetch('https://nanocodex.example'+path,{method,headers:{...(token?{authorization:'Bearer '+token}:{}),'content-type':'application/json',origin:'https://nanocodex.example',...headers},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const text=await response.text();let value;try{value=JSON.parse(text)}catch{value=text}
    const artifactValue = (path.startsWith('/__fixture') || path.includes('/credentials/claude/login'))
      ? {state:value?.state,error:value?.error,private_fields:'redacted'}
      : path.includes('/events/history') ? {event_count:value?.data?.length,has_more:value?.has_more,latest_cursor:value?.latest_cursor,
          tools:[...new Set((value?.data??[]).map(row=>row.event?.payload?.tool).filter(Boolean))],child_event_count:(value?.data??[]).filter(row=>row.agent_id!==undefined&&row.event).length}
      : path.includes('/turns') ? {state:value?.state,turn_id:value?.turn_id,receipt_present:true} : value;
    trace.push({method,path,status:response.status,value:artifactValue});assert.equal(response.status,status,JSON.stringify({...artifactValue,error:value?.error,message:value?.message}));return value;
  };
  const turn=async(agent,input,id,expected='completed')=>{
    const receipt=await call(`/v1/agents/${agent}/turns`,'POST',{input,id},202);
    let status;
    for(let n=0;n<600;n++){
      status=await call(`/v1/agents/${agent}/turns/${receipt.turn_id??id}`);
      if(['completed','failed','cancelled'].includes(status.state))break;
      await new Promise(r=>setTimeout(r,40));
    }
    assert.equal(status.state,expected,JSON.stringify(status));if(expected==='completed')assert.match(JSON.stringify(status),/CLAUDE_TOOL_DONE_/);return status;
  };
  const hostedCompletion=async({rootFamily,rootModel,childHarness,childModel,sourceOutcome='completed'})=>{
    let releaseChild, signalChild;
    const marker=`HOSTED_IDLE_${rootFamily.toUpperCase()}_${sourceOutcome.toUpperCase()}_ROOT`;
    const childOutput=`${marker}_CHILD_RESULT`;
    const rootOutput=`CLAUDE_TOOL_DONE_INTEGRATED: ${childOutput}`;
    hostedJourney={marker,childTask:`${marker}_CHILD_TASK submit your structured result`,childOutput,rootOutput,
      rootFamily,rootModel,childHarness,childModel,sourceOutcome,rootInitialCompleted:false,rootRequests:0,continuationRequests:0,childCalls:0,
      childGate:new Promise(resolve=>{releaseChild=resolve;releaseHostedChild=resolve;}),childEntered:()=>signalChild?.(),
      childEnteredPromise:new Promise(resolve=>{signalChild=resolve;})};
    const agent=(await call('/v1/agents','POST',rootFamily==='codex'?{}:{settings:{model:rootModel,thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
    if(rootFamily==='codex')await call(`/v1/agents/${agent}/routing`,'POST',{model:rootModel,thinking:'low'});
    const turnId=`journey-${marker.toLowerCase()}`;
    const accepted=await call(`/v1/agents/${agent}/turns`,'POST',{input:`${marker}: delegate a child and finish this turn before it completes`,id:turnId},202);
    let childDeadline;
    try { await Promise.race([hostedJourney.childEnteredPromise,new Promise((_,reject)=>{childDeadline=setTimeout(()=>reject(new Error(`${marker} child never reached its provider gate`)),30000);})]); }
    finally { clearTimeout(childDeadline); }
    if (sourceOutcome==='cancelled') {
      for(let n=0;n<150&&!hostedJourney.rootHeld;n++)await new Promise(resolve=>setTimeout(resolve,40));
      assert.equal(hostedJourney.rootHeld,true,'root remains active after spawning');
      await call(`/v1/agents/${agent}/turns/${turnId}/cancel`,'POST',undefined,202);
      let cancelled;
      for(let n=0;n<150;n++){
        cancelled=await call(`/v1/agents/${agent}/turns/${turnId}`);
        if(cancelled.state==='cancelled')break;
        await new Promise(resolve=>setTimeout(resolve,40));
      }
      assert.equal(cancelled.state,'cancelled');
      releaseChild();releaseHostedChild=undefined;
      let history;
      for(let n=0;n<150;n++){
        history=await call(`/v1/agents/${agent}/events/history?after=0&limit=256`);
        if(history.data.some(row=>row.agent_id!==undefined&&row.event?.type==='run.completed'))break;
        await new Promise(resolve=>setTimeout(resolve,40));
      }
      assert.ok(history.data.some(row=>row.agent_id!==undefined&&row.event?.type==='run.completed'),'child completes after root cancellation');
      // Observe enough event-loop turns for an erroneous asynchronous admission.
      for(let n=0;n<10;n++){
        await new Promise(resolve=>setTimeout(resolve,40));
        history=await call(`/v1/agents/${agent}/events/history?after=0&limit=256`);
        assert.equal(history.data.filter(row=>row.type==='turn_accepted').length,1,'completion does not undo root cancellation');
      }
      assert.equal(hostedJourney.continuationRequests,0);
      await writeFile(resolve(evidence,`${marker}.json`),JSON.stringify(history,null,2));
      trace.push({scenario:'hosted cancelled parent stays stopped',rootFamily,childHarness,child_completed:true,automatic_parent_turns:0});
      hostedJourney=undefined;
      return;
    }
    let initial;
    for(let n=0;n<600;n++){
      initial=await call(`/v1/agents/${agent}/turns/${accepted.turn_id??turnId}`);
      if(['completed','failed','cancelled'].includes(initial.state))break;
      await new Promise(resolve=>setTimeout(resolve,40));
    }
    assert.equal(initial.state,'completed',JSON.stringify(initial));
    assert.match(JSON.stringify(initial),/CLAUDE_TOOL_DONE_HOSTED_CHILD_DELEGATED/);
    const beforeRelease=await call(`/v1/agents/${agent}/events/history?after=0&limit=256`);
    assert.equal(beforeRelease.data.filter(row=>row.type==='turn_completed'&&row.id===turnId).length,1,'original root turn is complete while its child remains gated');
    const spawn=beforeRelease.data.find(row=>row.agent_id===undefined&&row.event?.type==='tool.result'&&row.event.payload.tool==='spawn_agent');
    assert.ok(spawn,JSON.stringify(beforeRelease));
    const childId=spawn.event.payload.structured_result.agent_id;
    assert.ok(childId,'the public root tool result identifies its managed child');
    assert.equal(beforeRelease.data.filter(row=>row.agent_id===childId&&row.event?.type==='tool.result'&&row.event.payload.tool==='submit_result').length,0,'child has not completed before release');
    releaseChild();releaseHostedChild=undefined;
    let completedHistory;
    for(let n=0;n<600;n++){
      completedHistory=await call(`/v1/agents/${agent}/events/history?after=0&limit=256`);
      if(completedHistory.data.some(row=>row.type==='turn_completed'&&row.final_message===rootOutput))break;
      await new Promise(resolve=>setTimeout(resolve,40));
    }
    const automatic=completedHistory.data.filter(row=>row.type==='turn_completed'&&row.final_message===rootOutput);
    assert.equal(automatic.length,1,'one idle-only managed continuation publishes one public result');
    assert.equal(hostedJourney.continuationRequests,2,'the automatic turn inspects the child then answers');
    assert.equal(hostedJourney.rootRequests,4,'two original requests and two continuation requests');
    assert.equal(hostedJourney.childCalls,2,'the child submits its result and completes one Code Mode follow-up');
    const childCompletions=completedHistory.data.filter(row=>row.agent_id===childId&&row.event?.type==='tool.result'&&row.event.payload.tool==='submit_result');
    assert.equal(childCompletions.length,1,'the direct child completion is integrated once');
    assert.equal(childCompletions[0].event.payload.structured_result.accepted,true);
    assert.match(automatic[0].final_message,new RegExp(childOutput),'the public automatic turn result carries the integrated child output');
    trace.push({scenario:'hosted automatic child completion',rootFamily,childHarness,root_turn_id:turnId,child_agent_id:childId,
      original_completed_while_child_gated:true,automatic_turn_id:automatic[0].turn_id,integrated_child_output:true,
      child_completion_count:childCompletions.length,automatic_inference_count:hostedJourney.continuationRequests});
    const listing=completedHistory.data.find(row=>row.agent_id===undefined&&row.turn_id===automatic[0].id&&row.event?.type==='tool.result'&&row.event.payload.tool==='list_agents');
    assert.equal(listing?.event.payload.structured_result.agents[0].status.output,childOutput,'automatic parent fetches the real completed child result');
    assert.equal(completedHistory.data.filter(row=>row.type==='turn_accepted').length,2,'exactly one autonomous turn is admitted');
    await writeFile(resolve(evidence,`${marker}.json`),JSON.stringify(completedHistory,null,2));
    const result={agent,childId,history:completedHistory};
    hostedJourney=undefined;
    return result;
  };
  try {
    mf=new Miniflare(options);
    token=(await call('/__fixture','POST',{user:identity})).token;
    assert.equal((await call('/v1/credentials')).claude.connected,false);
    assert.equal((await call('/v1/agents','POST',{},409)).error,'no_available_models');
    for(const capabilities of [['agents:read'],['agents:write']]) {
      const scoped=(await call('/__fixture','POST',{user:identity,capabilities})).token;
      for(const [path,method,body] of [['/v1/credentials/claude/login','POST',undefined],['/v1/credentials/claude/login','GET',undefined],['/v1/credentials/claude/login/complete','POST',{code:'synthetic-denied'}],['/v1/credentials/claude','DELETE',undefined]])await call(path,method,body,401,{authorization:'Bearer '+scoped});
    }
    for(const [path,method,body] of [['/v1/credentials/claude/login','POST',undefined],['/v1/credentials/claude/login','GET',undefined],['/v1/credentials/claude/login/complete','POST',{code:'synthetic-denied'}],['/v1/credentials/claude','DELETE',undefined]]) {
      const response=await mf.dispatchFetch('https://nanocodex.internal'+path,{method,headers:grantHeaders,...(body?{body:JSON.stringify(body)}:{})});assert.equal(response.status,401);trace.push({path,method,principal:'trusted ConnectGrant assertion',status:response.status});
    }
    assert.equal((await call('/v1/credentials/claude/login')).state,'signed_out');
    const login=await call('/v1/credentials/claude/login','POST');
    const state=new URL(login.authorization_url).searchParams.get('state');
    await call('/v1/credentials/claude/login/complete','POST',{code:`managed-runtime#${state}`});
    const credentials=await call('/v1/credentials');assert.equal(credentials.ready,true);assert.equal(credentials.claude.connected,true);
    assert.equal(credentials.openai.connected,false);assert.equal(credentials.chatgpt.connected,false);
    catalogUnsupportedOnly=true;
    const unsupportedOnly=await call('/v1/models');assert.deepEqual(unsupportedOnly.data,[]);assert.equal(unsupportedOnly.default_model,null);
    assert.equal(unsupportedOnly.availability.claude.connected,true);assert.equal(unsupportedOnly.availability.claude.available,false);assert.equal(unsupportedOnly.partial,false);
    await call('/v1/agents','POST',{},409);
    await call('/v1/agents','POST',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},409);
    catalogUnsupportedOnly=false;
    catalogOutage=true;
    assert.equal((await call('/v1/models','GET',undefined,503)).error,'model_availability_unavailable');
    assert.equal((await call('/v1/agents','POST',{},503)).error,'model_availability_unavailable');
    catalogOutage=false;
    const catalog=await call('/v1/models');assert.equal(catalog.availability.claude.available,true);assert.deepEqual(catalog.data.map(m=>m.id),['claude-sonnet-4-6','claude-opus-4-6']);assert.equal(catalog.default_model,'claude-sonnet-4-6');
    const beforeClaudeDefault=catalogRequests;
    const created=await call('/v1/agents','POST',{},201), agent=created.agent_id;
    assert.equal(catalogRequests-beforeClaudeDefault,2,'Claude-only default and admission share one paginated live catalog');
    assert.equal((await call(`/v1/agents/${agent}`)).settings.model,'claude-sonnet-4-6');
    // Exercise production admission and SSE over HTTP with the rebuilt Rust
    // WASM. Only the upstream Messages response is synthetic.
    progressAvailable=true;
    assert.ok((await call('/v1/models')).data.some(model=>model.id==='claude-opus-5-5'));
    const progressAgent=(await call('/v1/agents','POST',{settings:{model:'claude-opus-5-5',thinking:'high',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
    const progressInput={input:'Check the saved record and report progress.',id:'progress-sse'};
    await writeFile(resolve(evidence,'progress.input.json'),JSON.stringify(progressInput));
    const endpoint=new URL(`/v1/agents/${progressAgent}/turns`,await mf.ready);
    const {stdout:progressStatus}=await promisify(execFile)('curl',['--silent','--show-error','--max-time','60','--request','POST',
      '--header','authorization: Bearer '+token,'--header','content-type: application/json','--header','accept: text/event-stream',
      '--data-binary','@'+resolve(evidence,'progress.input.json'),'--dump-header',resolve(evidence,'progress.headers'),
      '--output',resolve(evidence,'progress.sse'),'--write-out','%{http_code}',endpoint.href]);
    assert.equal(progressStatus,'202');
    const progressWire=await readFile(resolve(evidence,'progress.sse'),'utf8');
    const progressEvents=progressWire.split('\n').filter(line=>line.startsWith('data: ')).map(line=>JSON.parse(line.slice(6)));
    const progressDeltas=progressEvents.filter(row=>row.event?.type==='reasoning.summary.delta');
    assert.equal(progressDeltas.map(row=>row.event.payload.text).join(''),'Checking the saved record.');
    assert.ok(progressDeltas.every(row=>row.event.payload.item_id==='progress-live:thinking:1'));
    assert.ok(progressWire.indexOf('reasoning.summary.delta')<progressWire.indexOf('tool.call'));
    assert.match(progressWire,/CLAUDE_TOOL_DONE_PROGRESS/);
    assert.doesNotMatch(progressWire,/PRIVATE_REASONING_SIGNATURE|PRIVATE_PROGRESS_SIGNATURE/);
    assert.equal(progressRequests.length,2);
    const progressHistory=await call(`/v1/agents/${progressAgent}/events/history?after=0&limit=256`);
    assert.equal(progressHistory.data.filter(row=>row.event?.type==='reasoning.summary.delta').map(row=>row.event.payload.text).join(''),'Checking the saved record.');
    trace.push({progress:{model:'claude-opus-5-5',effort:'high',display:'updates',public_sse:true,history:true,private_blocks_preserved:true}});
    progressAvailable=false;
    await call('/v1/models');
    const platform=(await call('/v1/agents','POST',{},201)).agent_id;
    await turn(platform,'SHARED_PLATFORM_NATIVE_PROBE save and read a synthetic memory','journey-shared-platform');
    const platformHistory=await call(`/v1/agents/${platform}/events/history?after=0&limit=256`);
    const platformResults=platformHistory.data.filter(row=>row.event?.type==='tool.result').map(row=>row.event.payload);
    for (const tool of ['environment','memories__write','memories__read'])
      assert.ok(platformResults.some(result=>result.tool===tool&&result.status==='completed'),`native ${tool} completed through the Worker API`);
    assert.match(JSON.stringify(platformHistory),/SHARED_PLATFORM_MEMORY_PROOF/);
    assert.ok(platformResults.some(result=>result.tool==='mcp__cua_repl__js'),'CUA dispatch is recorded in public events');
    assert.ok(platformResults.some(result=>result.tool==='wait'&&result.status==='completed'),'yielded Claude exec resumes through wait');
    // The mobile picker uses /routing, whose body excludes settings-only fields.
    await call(`/v1/agents/${agent}/routing`,'POST',{model:'claude-opus-4-6',thinking:'medium'});
    const selected=await call(`/v1/agents/${agent}`);
    assert.equal(selected.settings.model,'claude-opus-4-6');assert.equal(selected.settings.thinking,'medium');
    await call(`/v1/agents/${agent}/routing`,'POST',{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false},400);
    assert.equal((await call(`/v1/agents/${agent}`)).settings.model,'claude-opus-4-6','rejected mobile payload preserves selection');
    await call(`/v1/agents/${agent}/settings`,'PATCH',{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false});
    await turn(agent,'Write durable proof','journey-write');
    assert.equal(mcpStarts(),0,'native non-MCP tools never initialize the optional MCP adapter');
    await call(`/v1/agents/${agent}/settings`,'PATCH',{model:'claude-opus-4-6'},409);
    const done=await call(`/v1/agents/${agent}/done`,'PUT',{done:true});
    assert.equal(done.done,true);assert.ok(done.done_at>0);
    assert.equal((await call('/v1/agents')).summaries[agent].presentation.done,true);
    await mf.dispose(); mf=new Miniflare(options);
    const doneAfterReopen=(await call('/v1/agents')).summaries[agent].presentation;
    assert.equal(doneAfterReopen.done,true);assert.equal(doneAfterReopen.doneAt,done.done_at);
    await call(`/v1/agents/${agent}/done`,'PUT',{done:false});
    assert.equal((await call('/v1/agents')).summaries[agent].presentation.done,false);
    await turn(agent,'Read durable proof','journey-read');
    await mf.dispose(); mf=new Miniflare(options);
    await turn(agent,'Run exec_command durable proof','journey-exec-command');
    {
      // Attachments: images and inline PDFs reach Claude as native blocks.
      const media=(await call('/v1/agents','POST',{settings:{model:'claude-opus-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
      const png='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';
      const pdf='data:application/pdf;base64,'+Buffer.from('%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n').toString('base64');
      await call(`/v1/agents/${media}/turns`,'POST',{input:[{type:'file',file_data:'data:application/zip;base64,UEsDBA==',filename:'x.zip'}],id:'journey-media-invalid'},400);
      const images = [png, 'data:image/jpeg;base64,'+'/9j/4AAQSkZJRgABAQAASABIAAD/4QBMRXhpZgAATU0AKgAAAAgAAYdpAAQAAAABAAAAGgAAAAAAA6ABAAMAAAABAAEAAKACAAQAAAABAAAAAaADAAQAAAABAAAAAQAAAAD/7QA4UGhvdG9zaG9wIDMuMAA4QklNBAQAAAAAAAA4QklNBCUAAAAAABDUHYzZjwCyBOmACZjs+EJ+/8AAEQgAAQABAwEiAAIRAQMRAf/EAB8AAAEFAQEBAQEBAAAAAAAAAAABAgMEBQYHCAkKC//EALUQAAIBAwMCBAMFBQQEAAABfQECAwAEEQUSITFBBhNRYQcicRQygZGhCCNCscEVUtHwJDNicoIJChYXGBkaJSYnKCkqNDU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6g4SFhoeIiYqSk5SVlpeYmZqio6Slpqeoqaqys7S1tre4ubrCw8TFxsfIycrS09TV1tfY2drh4uPk5ebn6Onq8fLz9PX29/j5+v/EAB8BAAMBAQEBAQEBAQEAAAAAAAABAgMEBQYHCAkKC//EALURAAIBAgQEAwQHBQQEAAECdwABAgMRBAUhMQYSQVEHYXETIjKBCBRCkaGxwQkjM1LwFWJy0QoWJDThJfEXGBkaJicoKSo1Njc4OTpDREVGR0hJSlNUVVZXWFlaY2RlZmdoaWpzdHV2d3h5eoKDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uLj5OXm5+jp6vLz9PX29/j5+v/bAEMAAgICAgICAwICAwUDAwMFBgUFBQUGCAYGBgYGCAoICAgICAgKCgoKCgoKCgwMDAwMDA4ODg4ODw8PDw8PDw8PD//bAEMBAgICBAQEBwQEBxALCQsQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEP/dAAQAAf/aAAwDAQACEQMRAD8A/Syiiiv5XP4rP//Z',
        'data:image/gif;base64,'+'R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7',
        'data:image/webp;base64,'+'UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA'];
      const notes = 'Résumé: plain text survives replay.';
      await turn(media,[{type:'text',text:'MULTIMODAL_PROOF describe both'},...images.map(image_url=>({type:'image',image_url})),{type:'file',file_data:pdf,filename:'proof.pdf'},
        {type:'file',file_data:'data:text/plain;base64,'+Buffer.from(notes).toString('base64'),filename:'notes.txt'}],'journey-media');
      const sent=mediaRequests.at(-1).latest.content;
      assert.equal(mediaRequests.at(-1).model,'claude-opus-4-6');
      assert.ok(sent.some(block=>block.type==='text'&&block.text.includes('MULTIMODAL_PROOF')),JSON.stringify(sent).slice(0,2000));
      const image=sent.find(block=>block.type==='image');
      assert.deepEqual(image?.source,{type:'base64',media_type:'image/png',data:png.split(',')[1]});
      const document=sent.find(block=>block.type==='document');
      assert.deepEqual(document?.source,{type:'base64',media_type:'application/pdf',data:pdf.split(',')[1]});
      assert.equal(document.title,'proof.pdf');
      assert.deepEqual(sent.filter(block=>block.type==='image').map(block=>block.source.media_type), ['image/png','image/jpeg','image/gif','image/webp']);
      assert.deepEqual(sent.find(block=>block.title==='notes.txt')?.source,{type:'text',media_type:'text/plain',data:notes});
      await mf.dispose(); mf=new Miniflare(options);
      await turn(media,'MULTIMODAL_PROOF recall all attached documents','journey-media-reopen');
      const replay=mediaRequests.at(-1).messages.flatMap(message=>Array.isArray(message.content)?message.content:[]);
      for(const mime of ['image/png','image/jpeg','image/gif','image/webp','application/pdf','text/plain']) {
        assert.ok(replay.some(block=>block.source?.media_type===mime), `reopened history retains ${mime}`);
      }
      // Exercise the same multipart upload + descriptor sent by iOS/macOS.
      const attachmentId='01234567-89ab-4def-8123-456789abcdef';
      const original=Buffer.from('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7','base64');
      const attachmentPath=`/v1/agents/${media}/attachments/${attachmentId}`;
      const upload=await call(attachmentPath,'POST',{name:'animation.gif',media_type:'image/gif',size:original.length});
      const put=await mf.dispatchFetch('https://nanocodex.example'+attachmentPath+'/parts/1',{method:'PUT',headers:{authorization:'Bearer '+token,'content-type':'application/octet-stream','content-length':String(original.length)},body:original});
      assert.equal(put.status,200,await put.text());
      await call(attachmentPath+'/complete','POST');
      const descriptor='Attached original image file.\n[Image attachment]\n'+JSON.stringify({path:upload.path,media_type:'image/gif',preview_path:`/brain/attachments/${attachmentId}/preview.jpg`});
      await turn(media,[{type:'text',text:'MULTIMODAL_PROOF original upload'},{type:'text',text:descriptor}],'journey-original');
      assert.deepEqual(mediaRequests.at(-1).latest.content.find(block=>block.type==='image')?.source,{type:'base64',media_type:'image/gif',data:original.toString('base64')});
      const missing=descriptor.replaceAll(attachmentId,'01234567-89ab-4def-8123-456789abcdee');
      await turn(media,[{type:'text',text:'MULTIMODAL_PROOF missing upload'},{type:'text',text:missing}],'journey-missing');
      assert.match(JSON.stringify(mediaRequests.at(-1).latest.content),/Image attachment unavailable to Claude/);
      trace.push({claude_attachments:{native_formats:sent.filter(block=>block.source).map(block=>block.source.media_type),replayed:true,uploaded_original:'image/gif',missing:'explicit notice'}});
      // Streamed deltas and the final message share one response identity, so
      // transcript clients fold them into one row instead of rendering twice.
      const mediaHistory=await call(`/v1/agents/${media}/events/history?after=0&limit=256`);
      const assistantEvents=(mediaHistory.data??[]).map(row=>row.event).filter(event=>event?.type==='assistant.delta'||event?.type==='assistant.message');
      const finals=assistantEvents.filter(event=>event.type==='assistant.message');
      const deltas=assistantEvents.filter(event=>event.type==='assistant.delta');
      assert.ok(finals.length>=1&&deltas.length>=2,JSON.stringify(assistantEvents).slice(0,2000));
      for(const final of finals) assert.equal(deltas.filter(event=>event.payload.item_id===final.payload.item_id).map(event=>event.payload.text).join(''),final.payload.text,'streamed text equals the final message');
      for(const event of assistantEvents.filter(event=>event.type==='assistant.delta')) {
        assert.equal(typeof event.payload.item_id,'string','Claude deltas identify their provider message');
        assert.ok(finals.some(final=>final.payload.item_id===event.payload.item_id&&final.payload.model_call_index===event.payload.model_call_index),'each delta folds into its final message');
      }
      // GPT Realtime voice fronts the Claude Opus thread: lifecycle and
      // delegated transcripts reach Claude inline; stop retains the transcript.
      const voice=crypto.randomUUID();
      const started=await call(`/v1/agents/${media}/realtime/start`,'POST',{voice_session_id:voice,operation_id:'voice-start'});
      assert.match(JSON.stringify(started.context.history),/MULTIMODAL_PROOF describe both/);
      assert.match(JSON.stringify(started.context.history),/CLAUDE_TOOL_DONE_MEDIA_/);
      const delegated=await call(`/v1/agents/${media}/realtime/delegate`,'POST',{voice_session_id:voice,operation_id:'voice-delegate',
        input:'<realtime_delegation>\n  <input>MULTIMODAL_PROOF voice asks what the PDF was</input>\n</realtime_delegation>'},202);
      assert.equal(delegated.route,'started',JSON.stringify(delegated));
      for(let n=0;n<600;n++){const state=(await call(`/v1/agents/${media}/turns/${delegated.turn_id}`)).state;if(state==='completed')break;assert.ok(!['failed','cancelled'].includes(state),state);await new Promise(r=>setTimeout(r,40));}
      const voiceTurn=JSON.stringify(mediaRequests.at(-1).latest.content);
      assert.match(voiceTurn,/Realtime conversation started/);assert.match(voiceTurn,/voice asks what the PDF was/);
      const stopped=await call(`/v1/agents/${media}/realtime/stop`,'POST',{voice_session_id:voice,operation_id:'voice-stop',transcript:[{role:'user',text:'VOICE_TRANSCRIPT_PROOF spoken only'}]});
      assert.equal(stopped.stopped,true);
      await turn(media,'MULTIMODAL_PROOF typed after voice','journey-media-after-voice');
      const afterVoice=JSON.stringify(mediaRequests.at(-1).latest.content);
      assert.match(afterVoice,/VOICE_TRANSCRIPT_PROOF spoken only/);assert.match(afterVoice,/Realtime conversation ended/);
      assert.doesNotMatch(afterVoice,/Realtime conversation started/,'queued context is consumed exactly once');
      trace.push({claude_media_voice:{blocks:sent.map(block=>block.type),voice_route:delegated.route,stopped:stopped.stopped}});
    }
    const history=await call(`/v1/agents/${agent}/events/history?after=0&limit=256`);assert.match(JSON.stringify(history),/Write|Read|exec_command/);
    await call(`/v1/agents/${agent}/durability`,'POST',undefined,409);
    await call(`/v1/agents/${agent}/forks`,'POST',undefined,409,{'idempotency-key':'claude-fork-denial'});
    // Script the external model for the requested operation, without coupling
    // the fixture to the runtime's exact compaction-prompt wording.
    compacting=true;
    try { await call(`/v1/agents/${agent}/compact`,'POST'); }
    finally { compacting=false; }
    assert.equal(summaries,1);
    await mf.dispose(); mf=new Miniflare(options);
    await turn(agent,'Read durable proof after summary','journey-after-summary');
    assert.equal(writes,1,'compaction/reopen never repeats prior effect');
    const commandsOnly=(await call('/v1/agents','POST',{configuration:{tools:['exec_command','write_stdin']}},201)).agent_id;
    await turn(commandsOnly,'CANONICAL_COMMAND_PROBE','journey-canonical-command');
    for (const name of ['Bash','BashOutput']) {
      const legacy=(await call('/v1/agents','POST',{configuration:{tools:[name]}},201)).agent_id;
      const beforeLegacy=calls;
      assert.match(JSON.stringify(await turn(legacy,'Legacy shell must fail before inference','journey-removed-'+name,'failed')),/configuration names an unavailable tool/);
      assert.equal(calls,beforeLegacy);
    }
    const noTools=(await call('/v1/agents','POST',{configuration:{tools:[]}},201)).agent_id;
    await turn(noTools,'Try forbidden Write','journey-no-tools');
    await turn(agent,'Check denied file','journey-denied-file');
    const onlyWrite=(await call('/v1/agents','POST',{configuration:{tools:['Write']}},201)).agent_id;
    await turn(onlyWrite,'Try forbidden Read','journey-write-only');
    assert.equal(mcpStarts(),0,'empty and non-MCP allowlists do not start MCP discovery');
    const beforeMcp = mcpStarts();
    const mcpAgent=(await call('/v1/agents','POST',{},201)).agent_id;
    await turn(mcpAgent,'MCP_LAZY_SEARCH proof','journey-mcp-search');
    assert.equal(mcpStarts()-beforeMcp,6,'search and execute share one set of public MCP clients');
    const mcpHistory=await call(`/v1/agents/${mcpAgent}/events/history?after=0&limit=256`);
    assert.match(JSON.stringify(mcpHistory),/MCP_LAZY_PUBLIC_PROOF/);
    const directMcp=(await call('/v1/agents','POST',{},201)).agent_id;
    await turn(directMcp,'MCP_LAZY_DIRECT proof','journey-mcp-direct');
    assert.equal(mcpStarts()-beforeMcp,12,'execute as first MCP action discovers before exact resolution');
    await call(`/v1/agents/${mcpAgent}`,'DELETE',undefined,204);
    await call(`/v1/agents/${directMcp}`,'DELETE',undefined,204);
    const removedTasks=(await call('/v1/agents','POST',{configuration:{tools:['Task','TaskOutput','TaskStop'],multi_agent:{enabled:true}}},201)).agent_id;
    const callsBeforeRemovedTasks=calls;
    assert.match(JSON.stringify(await turn(removedTasks,'Legacy delegation must fail before inference','journey-removed-tasks','failed')),/capability Task was removed.*canonical subagent tools/);
    assert.equal(calls,callsBeforeRemovedTasks,'legacy delegation configuration cannot reach inference or create an alias child');
    const closingMcp=(await call('/v1/agents','POST',{},201)).agent_id;
    holdMcp=true; mcpHold=new Promise(resolve=>{releaseMcp=resolve;});
    const heldMcpStart=mcpTrace.length;
    await call(`/v1/agents/${closingMcp}/turns`,'POST',{input:'MCP_LAZY_SEARCH hold',id:'journey-mcp-close-during-init'},202);
    for(let n=0;n<150 && mcpTrace.slice(heldMcpStart).filter(row=>row.method==='initialize').length<6;n++)await new Promise(resolve=>setTimeout(resolve,20));
    assert.equal(mcpTrace.slice(heldMcpStart).filter(row=>row.method==='initialize').length,6);
    let closeDeadline;
    try {
      await Promise.race([call(`/v1/agents/${closingMcp}`,'DELETE',undefined,204),new Promise((_,reject)=>{closeDeadline=setTimeout(()=>reject(new Error('MCP teardown waited for withheld discovery')),5000);})]);
    } finally { clearTimeout(closeDeadline); holdMcp=false; releaseMcp(); }
    // A completed unrelated HTTP request gives released callbacks an opportunity
    // to run without imposing a timing-based correctness threshold.
    await call('/v1/credentials');
    assert.equal(mcpTrace.slice(heldMcpStart).filter(row=>row.method==='tools/list'||row.method==='tools/call').length,0,'closed discovery cannot proceed to listing or execution');
    const canonical=(await call('/v1/agents','POST',{},201)).agent_id;
    await turn(canonical,'Delegate canonical Claude child','journey-canonical-child');
    assert.equal(canonicalWrites,1);
    const canonicalHistory=await call(`/v1/agents/${canonical}/events/history?after=0&limit=256`);
    assert.equal(canonicalHistory.data.filter(row=>row.agent_id===undefined&&row.event?.type==='run.started').length,1,'active wait produces one root run without a redundant continuation');
    assert.ok(canonicalHistory.data.some(row=>row.agent_id!==undefined && row.event?.type==='tool.result' && row.event.payload.tool==='Write'),'canonical child execution appears in public child events');
    assert.match(JSON.stringify(canonicalHistory),/CANONICAL_DURABLE_CHILD_PROOF/);
    const unavailableCanonical=(await call('/v1/agents','POST',{},201)).agent_id;
    await turn(unavailableCanonical,'Try unavailable canonical child','journey-canonical-unavailable');
    assert.equal(canonicalWrites,1,'unavailable model never reaches child tools');
    const disabledCanonical=(await call('/v1/agents','POST',{configuration:{multi_agent:{enabled:false}}},201)).agent_id;
    await turn(disabledCanonical,'Try disabled canonical child','journey-canonical-disabled');
    assert.equal(canonicalWrites,1,'disabled child never executes');
    assert.deepEqual(deniedCalls,['Write','Read','spawn_agent'],'every unadmitted call completes through a paired error');
    const held=await call(`/v1/agents/${agent}/turns`,'POST',{input:'Hold until cancelled',id:'journey-cancel'},202);
    for(let n=0;n<150&&!holds;n++)await new Promise(r=>setTimeout(r,40));assert.equal(holds,1);
    await call(`/v1/agents/${agent}/turns/${held.turn_id??'journey-cancel'}/cancel`,'POST',undefined,202);
    let cancelled;for(let n=0;n<150;n++) { cancelled=await call(`/v1/agents/${agent}/turns/journey-cancel`);if(cancelled.state==='cancelled')break;await new Promise(r=>setTimeout(r,40)); }
    assert.equal(cancelled.state,'cancelled');
    await mf.dispose(); mf=new Miniflare(options);
    assert.equal((await call(`/v1/agents/${agent}/turns/journey-cancel`)).state,'cancelled');
    await call(`/v1/agents/${canonical}/turns`,'POST',{input:'Delegate canonical Claude child',id:'journey-canonical-child'},200);
    assert.equal(canonicalWrites,1,'completed canonical child not replayed after restart');
    await turn(agent,'Run exec_command durable proof after cancellation','journey-after-cancel');assert.equal(holds,1,'cancelled request not replayed');

    {
      // HTTP + GPT Realtime delegation both steer one active Rust/WASM Claude turn.
      const steered=(await call('/v1/agents','POST',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
      const activeVoice=crypto.randomUUID();
      await call(`/v1/agents/${steered}/realtime/start`,'POST',{voice_session_id:activeVoice,operation_id:'active-voice-start'});
      await call(`/v1/agents/${steered}/turns`,'POST',{input:'MANAGED_ACTIVE_STEER wait for both corrections',id:'journey-active-steer'},202);
      for(let n=0;n<150&&!releaseActiveSteer;n++)await new Promise(r=>setTimeout(r,40));
      assert.equal(typeof releaseActiveSteer,'function','model request started and is held at the terminal boundary');
      const first='first managed steering correction', second='<realtime_delegation>\n<input>second managed é correction</input>\n</realtime_delegation>';
      const steerPath=`/v1/agents/${steered}/turns/journey-active-steer`;
      await call(`${steerPath}/steer`,'POST',{input:'WITHDRAWN_CORRECTION_MUST_NOT_REACH_MODEL',message_id:'withdrawn-steer'},202);
      const withdrawal=await call(`${steerPath}/withdraw-steer`,'POST',{message_id:'withdrawn-steer'});
      assert.equal(withdrawal.withdrawn,true,'pending identified Claude steering can be withdrawn');
      assert.equal((await call(`${steerPath}/steer-receipt?message_id=withdrawn-steer`)).state,'withdrawn');
      const correction={input:first,message_id:'tui-steer-first'};
      await call(`${steerPath}/steer`,'POST',correction,202);
      await call(`${steerPath}/steer`,'POST',correction,202);
      const conflict=await call(`${steerPath}/steer`,'POST',{input:'different correction',message_id:correction.message_id},409);
      assert.equal(conflict.error,'message_id_conflict','identified steering never reuses an ID for different input');
      const pendingReceipt=await call(`${steerPath}/steer-receipt?message_id=${correction.message_id}`);
      assert.equal(pendingReceipt.state,'accepted');
      assert.equal(pendingReceipt.terminal,false);
      assert.equal(pendingReceipt.input_key,createHash('sha256').update(JSON.stringify({instruction:first})).digest('hex'),'receipt fingerprints the exact browser input');
      const delegated=await call(`/v1/agents/${steered}/realtime/delegate`,'POST',{
        voice_session_id:activeVoice,operation_id:'active-voice-delegate',input:second},202);
      assert.equal(delegated.route,'steered',JSON.stringify(delegated));
      assert.equal(delegated.turn_id,'journey-active-steer','voice attribution stays on the existing active turn');
      const queued=await call(`/v1/agents/${steered}/events/history?after=0&limit=256`);
      assert.equal(queued.data.filter(row=>row.event?.type==='run.steered').length,0,'admission is not consumption');
      assert.equal(activeSteerRequests.length,1,'queued steering cannot start concurrent model inference');
      releaseActiveSteer();
      let result;for(let n=0;n<600;n++) {
        result=await call(`/v1/agents/${steered}/turns/journey-active-steer`);
        if(['completed','failed','cancelled'].includes(result.state))break;
        await new Promise(r=>setTimeout(r,40));
      }
      assert.equal(result.state,'completed',JSON.stringify(result));
      assert.match(JSON.stringify(result),/CLAUDE_TOOL_DONE_ACTIVE_STEER/);
      const consumed=await call(`/v1/agents/${steered}/events/history?after=0&limit=256`);
      const acknowledgements=consumed.data.filter(row=>row.event?.type==='run.steered');
      assert.equal(acknowledgements.length,2,'duplicate admission and withdrawal never duplicate consumption');
      assert.equal(acknowledgements[0].event.payload.message_id,correction.message_id,'native Claude consumption acknowledges the TUI identity');
      assert.equal(acknowledgements[0].event.payload.instruction_bytes,Buffer.byteLength(first));
      assert.ok(acknowledgements[1].event.payload.instruction_bytes>=Buffer.byteLength(second),'voice delegation retains its origin context');
      assert.ok(JSON.stringify(activeSteerRequests[1]).includes(JSON.stringify(second).slice(1,-1)),'voice steering preserves the complete instruction');
      assert.equal(activeSteerRequests.length,2,'one initial request and one ordered continuation');
      assert.ok(!JSON.stringify(activeSteerRequests).includes('WITHDRAWN_CORRECTION_MUST_NOT_REACH_MODEL'));
      const terminalReceipt=await call(`${steerPath}/steer-receipt?message_id=${correction.message_id}`);
      assert.equal(terminalReceipt.state,'accepted');
      assert.equal(terminalReceipt.input_key,pendingReceipt.input_key);
      assert.equal(terminalReceipt.terminal,true);
      await call(`${steerPath}/steer`,'POST',correction,202);
      assert.equal((await call(`${steerPath}/withdraw-steer`,'POST',{message_id:correction.message_id})).withdrawn,false,'a consumed correction cannot be withdrawn');
      const terminalIndex=consumed.data.findIndex(row=>row.event?.type==='run.completed');
      assert.ok(terminalIndex>=0 && acknowledgements.every(row=>consumed.data.indexOf(row)<terminalIndex),'consumption precedes terminal completion');
      const stopped=await call(`/v1/agents/${steered}/realtime/stop`,'POST',{
        voice_session_id:activeVoice,operation_id:'active-voice-stop',transcript:[{role:'user',text:'ACTIVE_VOICE_TRANSCRIPT_PROOF'}]});
      assert.equal(stopped.stopped,true);
      assert.match(JSON.stringify(activeSteerRequests[0]),/Realtime conversation started/);
      trace.push({scenario:'active Claude steering',requests:activeSteerRequests.length,steer_indices:acknowledgements.map(row=>row.event.payload.steer_index),ordered:true,voice_route:delegated.route,voice_stopped:stopped.stopped,identified_steering:true,pending_withdrawal:true,duplicate_consumption:false,terminal_receipt:true});

      // CR-only SSE frames are valid; EOF without message_stop is not completion.
      const frames=[];
      for(const [marker,id,expected] of [['MANAGED_FRAME_CR_ONLY','journey-frame-cr','completed'],['MANAGED_FRAME_TRUNCATED','journey-frame-truncated','failed']]) {
        const framed=(await call('/v1/agents','POST',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
        const result=await turn(framed,marker,id,expected);
        if(expected==='failed')assert.match(JSON.stringify(result),/Messages stream ended before message_stop/);
        const events=await call(`/v1/agents/${framed}/events/history?after=0&limit=256`);
        assert.equal(events.data.filter(row=>row.event?.type==='run.completed').length,expected==='completed'?1:0,'truncated stream cannot publish successful terminal');
        frames.push({agent:framed,id,expected,input:marker});
      }
      assert.deepEqual(framingRequests,{crOnly:1,truncated:5},'an unpublished truncated stream exhausts the transient retry budget');
      await mf.dispose();mf=new Miniflare(options);
      for(const {agent,id,expected,input} of frames) {
        assert.equal((await call(`/v1/agents/${agent}/turns/${id}`)).state,expected);
        await call(`/v1/agents/${agent}/turns`,'POST',{input,id},200);
      }
      assert.deepEqual(framingRequests,{crOnly:1,truncated:5},'retained failure and completion survive restart without replay');
      trace.push({scenario:'managed WASM SSE framing',...framingRequests,crOnly:'completed',missingMessageStop:'failed',automaticRetries:4,restartReplay:false});
    }

    const beforeDeniedMcp=mcpStarts(), ownerMcpToken=token;
    token=(await call('/__fixture','POST',{user:identity,capabilities:['agents:read','agents:write']})).token;
    await call(`/v1/agents/${agent}/turns`,'POST',{input:'MCP_LAZY_SEARCH denied',id:'journey-mcp-denied'},403);
    token=ownerMcpToken;
    assert.equal(mcpStarts(),beforeDeniedMcp,'denied tool authority performs no MCP I/O');
    const beforeGrant=calls;
    for(const [path,body] of [['/v1/models',undefined],['/v1/agents',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}}],[`/v1/agents/${agent}/turns`,{input:'Write durable proof',id:'connect-denied'}]]) {
      const response=await mf.dispatchFetch('https://nanocodex.internal'+path,{method:body?'POST':'GET',headers:grantHeaders,...(body?{body:JSON.stringify(body)}:{})});
      assert.equal(response.status,403);trace.push({path,principal:'trusted ConnectGrant assertion',status:response.status,value:await response.json()});
    }
    assert.equal(calls,beforeGrant,'ConnectGrant denied before Messages forwarding');
    await call('/v1/agents','POST',{configuration:{tools:['WebSearch']}},409);
    await call('/v1/credentials/claude','DELETE');assert.equal((await call('/v1/credentials')).claude.connected,false);
    assert.equal((await call('/v1/models')).data.length,0);
    await call('/v1/agents','POST',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},409);
    // Reconnect the same real broker flow, then seed only the unavoidable
    // synthetic OpenAI credential boundary. No real provider or secret is used.
    const reconnect=await call('/v1/credentials/claude/login','POST');
    await call('/v1/credentials/claude/login/complete','POST',{code:'managed-runtime#'+new URL(reconnect.authorization_url).searchParams.get('state')});
    await call('/__fixture/openai','POST',undefined,204);catalogOutage=true;
    const mixed=await call('/v1/models');assert.equal(mixed.partial,true);assert.equal(mixed.availability.claude.error,'claude_models_unavailable');
    assert.deepEqual(mixed.data.map(model=>model.id),['gpt-6-astra','gpt-6.1-sol','gpt-6-luna']);
    await turn(agent,'Run exec_command durable proof in mixed account','journey-mixed-provider-pin');assert.equal(responsesAttempts,0,'Claude inference/sidebar cannot borrow OAI credential');
    const beforeGptMcp=mcpStarts();
    const gptMcp=(await call('/v1/agents','POST',{settings:{model:'gpt-6.1-sol',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
    await turn(gptMcp,'GPT_MCP_DISCOVERY_PROBE','journey-gpt-mcp');
    assert.equal(mcpStarts()-beforeGptMcp,6,'GPT-only turn starts primary discovery once, with no alternate-Claude duplicate');
    await call(`/v1/agents/${gptMcp}`,'DELETE',undefined,204);
    allowResponses=true;
    const reverse=(await call('/v1/agents','POST',{settings:{model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
    catalogOutage=false;
    const mobileSelection=await call(`/v1/agents/${reverse}/routing`,'POST',{model:'claude-sonnet-4-6',thinking:'low'});
    assert.equal(mobileSelection.automatic,false);assert.equal(mobileSelection.settings.model,'claude-sonnet-4-6');
    await hostedCompletion({rootFamily:'claude',rootModel:'claude-sonnet-4-6',childHarness:'codex',childModel:'gpt-6.1-sol'});
    await hostedCompletion({rootFamily:'claude',rootModel:'claude-sonnet-4-6',childHarness:'codex',childModel:'gpt-6.1-sol',sourceOutcome:'cancelled'});
    await turn(reverse,'Delegate canonical Codex child','journey-reverse-child');
    assert.equal(codexWrites,1,'explicit Claude→Codex delegation executes one real Code Mode effect');
    assert.equal((await call(`/v1/agents/${reverse}`)).settings.model,'claude-sonnet-4-6');
    assert.equal((await call(`/v1/agents/${reverse}/routing`,'POST',{model:'claude-opus-4-6',thinking:'low'},409)).error,'routing_requires_new_thread');
    allowResponses=false;
    catalogOutage=true;
    assert.ok(!mixed.data.find(model=>model.id==='gpt-6-astra').thinking.includes('none'));assert.ok(mixed.data.find(model=>model.id==='gpt-6-luna').thinking.includes('none'));
    const beforeMixedDefault=catalogRequests;
    const mixedDefault=await call('/v1/agents','POST',{},201);assert.equal((await call(`/v1/agents/${mixedDefault.agent_id}`)).settings.model,'gpt-6-astra');
    assert.equal(catalogRequests,beforeMixedDefault+1,'default admission checks Claude before falling back to OpenAI');
    catalogOutage=false;
    let releaseCatalog, deadline;
    catalogHold=new Promise(resolve=>{releaseCatalog=resolve;});
    try {
      const heldDefault=await Promise.race([
        call('/v1/agents','POST',{settings:{model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201),
        new Promise((_,reject)=>{deadline=setTimeout(()=>reject(new Error('default admission waited for the withheld Claude catalog')),5000);}),
      ]);
      assert.equal((await call(`/v1/agents/${heldDefault.agent_id}`)).settings.model,'gpt-6-astra');
      assert.equal(catalogRequests,beforeMixedDefault+1,'explicit OpenAI admission does not start a Claude lookup');
      trace.push({scenario:'both providers, Claude catalog withheld',default_model:'gpt-6-astra',catalog_requests:catalogRequests-beforeMixedDefault,completed_before_catalog_release:true});
    } finally { clearTimeout(deadline); releaseCatalog(); catalogHold=undefined; }
    const fullMixed=await call('/v1/models');
    assert.equal(fullMixed.partial,false);
    assert.deepEqual(fullMixed.data.map(model=>model.id),['gpt-6-astra','gpt-6.1-sol','gpt-6-luna','claude-sonnet-4-6','claude-opus-4-6']);
    assert.equal(fullMixed.default_model,'gpt-6-astra');
    assert.equal(catalogRequests-beforeMixedDefault,3,'GET models still reads every Claude catalog page');
    assert.equal((await call('/v1/agents','POST',{settings:{model:'claude-sonnet-5-5',thinking:'low',reasoning_mode:'standard',fast_mode:false}},409)).error,'claude_model_unavailable');
    catalogOutage=true;
    await call('/v1/agents','POST',{settings:{model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201);
    // Creation reuses the last successful Claude listing (egress isolate cache), so a later provider catalog outage does not block it.
    await call('/v1/agents','POST',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201);
    await mf.dispose();options.workers[0].bindings.NANOCODEX_THREAD_ROUTING='true';options.workers[0].bindings.OPENROUTER_API_KEY='synthetic-gateway-key';options.workers[0].ai={binding:'AI'};mf=new Miniflare(options);
    const ownerToken=token;
    token=(await call('/__fixture','POST',{user:'11111111-1111-4111-8111-111111111144'})).token;
    const gatewayCredentials=await call('/v1/credentials');for(const name of ['openai','chatgpt','claude'])assert.equal(gatewayCredentials[name].connected,false);
    const gatewayOnly=await call('/v1/models');assert.deepEqual(gatewayOnly.data.map(model=>model.id),['@cf/zai-org/glm-5.3','kimi-k3','mimo-v2.6-pro']);
    assert.equal(gatewayOnly.default_model,'@cf/zai-org/glm-5.3');assert.equal(gatewayOnly.availability.claude.available,false);
    // Reproduce the two user questions through the public manual-routing API,
    // whose managed instructions replace the model's built-in prompt.
    const mimo=(await call('/v1/agents','POST',{},201)).agent_id;
    await call(`/v1/agents/${mimo}/routing`,'POST',{model:'mimo-v2.6-pro',thinking:'low'});
    assert.equal((await call(`/v1/agents/${mimo}`)).settings.model,'mimo-v2.6-pro');
    await turn(mimo,'MODEL_IDENTITY_PROBE: which model r u','journey-mimo-identity');
    await mf.dispose();mf=new Miniflare(options);
    await turn(mimo,'MODEL_IDENTITY_PROBE: arent u mimo','journey-mimo-identity-reopen');
    const identityHistory=await call(`/v1/agents/${mimo}/events/history?after=0&limit=256`);
    assert.match(JSON.stringify(identityHistory),/CLAUDE_TOOL_DONE_MODEL_IDENTITY: mimo-v2.6-pro/);
    await call(`/v1/agents/${mimo}`,'DELETE',undefined,204);
    token=ownerToken;
    const legacy=await call('/v1/models');for(const model of ['@cf/zai-org/glm-5.3','kimi-k3','mimo-v2.6-pro'])assert.ok(legacy.data.some(row=>row.id===model));
    const gateway=await call('/v1/agents','POST',{},201);await call(`/v1/agents/${gateway.agent_id}/routing`,'POST',{model:'kimi-k3',thinking:'low'});assert.equal((await call(`/v1/agents/${gateway.agent_id}`)).settings.model,'kimi-k3');
    catalogOutage=false;
    await hostedCompletion({rootFamily:'codex',rootModel:'kimi-k3',childHarness:'claude',childModel:'claude-opus-4-6'});
    await turn(gateway.agent_id,'Delegate mixed Claude child','journey-mixed-child');
    assert.equal(canonicalWrites,2,'Codex-family gateway root executes native Claude child');
    const mixedHistory=await call(`/v1/agents/${gateway.agent_id}/events/history?after=0&limit=256`);
    assert.ok(mixedHistory.data.some(row=>row.agent_id!==undefined && row.event?.type==='tool.result' && row.event.payload.tool==='Write'),'mixed child Write is attributed to a public child event');
    assert.equal((await call(`/v1/agents/${gateway.agent_id}`)).settings.model,'kimi-k3','child selection never changes the root backend');
    const nested=(await call('/v1/agents','POST',{},201)).agent_id;
    await call(`/v1/agents/${nested}/routing`,'POST',{model:'kimi-k3',thinking:'low'});
    const nestedResult=await turn(nested,'Delegate nested gateway grandchild','journey-nested-gateway');
    assert.match(JSON.stringify(nestedResult),/GATEWAY_GRANDCHILD_DURABLE_PROOF/,'grandchild proof reaches the public root turn result');
    assert.equal(nestedWrites,1,'gateway grandchild executes one real Code Mode filesystem effect');
    const nestedHistory=await call(`/v1/agents/${nested}/events/history?after=0&limit=256`);
    await writeFile(resolve(evidence,'nested-public-history.json'),JSON.stringify(nestedHistory,null,2));
    const nestedTools=nestedHistory.data.filter(row=>row.event?.type==='tool.result');
    const claudeChildId=nestedTools.find(row=>row.agent_id===undefined&&row.event.payload.tool==='spawn_agent').event.payload.structured_result.agent_id;
    const gatewayGrandchildId=nestedTools.find(row=>row.agent_id===claudeChildId&&row.event.payload.tool==='spawn_agent').event.payload.structured_result.agent_id;
    assert.notEqual(gatewayGrandchildId,claudeChildId);
    const grandchildEffect=nestedTools.find(row=>row.agent_id===gatewayGrandchildId&&row.event.payload.tool==='exec_command').event.payload.structured_result;
    assert.equal(grandchildEffect.exit_code,0);
    assert.equal(grandchildEffect.output,'GATEWAY_GRANDCHILD_DURABLE_PROOF','grandchild filesystem effect is visible in public events');
    const grandchildReceipt=nestedTools.find(row=>row.agent_id===claudeChildId&&row.event.payload.tool==='wait_agent').event.payload.structured_result.agents[0];
    assert.equal(grandchildReceipt.agent_id,gatewayGrandchildId);
    assert.equal(grandchildReceipt.parent_agent_id,claudeChildId,'public receipt confirms the three-generation hierarchy');
    for (const id of [claudeChildId,gatewayGrandchildId]) assert.equal(nestedTools.find(row=>row.agent_id===id&&row.event.payload.tool==='submit_result').event.payload.structured_result.accepted,true);
    assert.equal((await call(`/v1/agents/${nested}`)).settings.model,'kimi-k3');
    await call('/v1/credentials/claude','DELETE');catalogOutage=false;
    const beforeDisconnected=calls;
    await turn(gateway.agent_id,'Try disconnected mixed child','journey-mixed-disconnected');
    assert.equal(calls,beforeDisconnected,'disconnect prevents Claude child inference even with an existing capability');
    assert.equal(canonicalWrites,2);
    const staged=await call('/v1/credentials/claude/login','POST');
    const validating=await call('/v1/credentials/claude/login/complete','POST',{code:'profile-uncertain#'+new URL(staged.authorization_url).searchParams.get('state')},400);assert.equal(validating.state,'validating');
    await mf.dispose();mf=new Miniflare(options);
    assert.equal((await call('/v1/credentials/claude/login')).state,'authenticated');assert.equal((await call('/v1/credentials')).claude.connected,true);
    assert.ok((await call('/v1/models')).data.some(row=>row.id==='claude-sonnet-4-6'));
    const validationTrace=await (await claudeProvider(new Request('https://claude-fixture.invalid/trace?scenario=profile-uncertain'))).json();assert.equal(validationTrace.exchange,1);assert.equal(validationTrace.profile,2);
    assert.deepEqual(providerErrors,[],"all provider fixtures matched the real public journeys");
    console.info('CLAUDE_MANAGED_JOURNEY',{calls,summaries,writes,canonicalWrites,codexWrites,nestedWrites,sidebarCalls,holds,responsesAttempts,DOReopens:5,framingRequests,activeSteerRequests:activeSteerRequests.length,modelTools:['exec','wait'],nestedTools:['Write','Read','exec_command','write_stdin'],actualModels:catalog.data.map(m=>m.id),staleSelectionDenied:true,gatewayOnlyDefault:gatewayOnly.default_model,unsupportedOnlyAvailable:unsupportedOnly.availability.claude.available,exactToolAllowlist:true});
  } finally {
    releaseActiveSteer?.();
    releaseHostedChild?.();
    await mf?.dispose();
    await writeFile(resolve(evidence,'public-api-trace.json'),JSON.stringify(trace,null,2));
    await writeFile(resolve(evidence,'provider-trace.json'),JSON.stringify(upstream,null,2));
    await writeFile(resolve(evidence,'provider-errors.json'),JSON.stringify(providerErrors,null,2));
    await writeFile(resolve(evidence,'mcp-trace.json'),JSON.stringify(mcpTrace,null,2));
    await writeFile(resolve(evidence,'steer-framing-trace.json'),JSON.stringify({activeSteerRequests,framingRequests},null,2));
    await rm(persistence,{recursive:true,force:true});
  }
});
