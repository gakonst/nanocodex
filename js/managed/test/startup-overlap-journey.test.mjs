import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";

// Actual account HTTP proxy/authentication, Managed Session, SQLite, R2,
// Just Bash, SDK and WASM. Only the external account metadata/model/HTTP target
// are synthetic. Catalog, Vault and configured setup remain externally gated
// until the first public answer; mandatory startup joins cannot pass by timing.
// Registration fault injection wraps the actual UserAccount HTTP boundary: one
// publish is held until the first answer, then fails once before the normal
// background publication retries against the production account DO.
const candidateRoot = fileURLToPath(new URL("..", import.meta.url));
const root = process.env.NANOCODEX_STARTUP_SOURCE_ROOT ?? candidateRoot;
const output = join(candidateRoot, "../../output/startup-overlap-journey", `${Date.now()}-${process.pid}`);
const source = `
import { DurableObject, WorkerEntrypoint } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools } from './src/index.ts';
import { UserAccount as RealUserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, revokeApiKey } from './src/account-auth.ts';
import { routeManaged } from '../account/worker/managedProxy.ts';
export { DurableAgentSession, AccountHostedTools, Organization, ApiKeyRecord, NonceStorage };
export class OriginAgentSession extends DurableAgentSession {
  async fetch(request) {
    const url=new URL(request.url);
    if(url.pathname==='/create-live') {
      // Exercise the actual handler while its dispatch yield is pending, using
      // the authenticated public request. No production delay/test hook.
      const first=super.fetch(request.clone());
      const competitors=await Promise.all([
        super.fetch(request.clone()),
        super.fetch(new Request('https://session.internal/create-run',{method:'POST',headers:request.headers,body:'{}'})),
        super.fetch(new Request('https://session.internal/initialize',{method:'PUT',body:'{}'})),
      ]);
      const statuses=competitors.map(response=>response.status);
      const response=await first;
      console.info({type:'fixture.admission_race',statuses,winner:response.status});
      if(statuses.some(status=>status!==409)) throw Error('competing admission was not fenced');
      return response;
    }
    if(url.pathname==='/fixture-origin-state') {
      const turn=url.searchParams.get('turn');
      return Response.json({
        dispatch:this.ctx.storage.sql.exec('SELECT input_json FROM managed_turn_dispatch_chunks WHERE turn_id = ? ORDER BY chunk_index',turn).toArray().map(row=>row.input_json).join(''),
        startup:this.ctx.storage.sql.exec('SELECT environment_json FROM managed_startup_environment WHERE turn_id = ?',turn).toArray(),
        context:this.ctx.storage.sql.exec('SELECT content FROM managed_startup_context WHERE turn_id = ?',turn).toArray(),
      });
    }
    return super.fetch(request);
  }
}
export class OriginAccountHostedTools extends AccountHostedTools {
  async fetch(request) {
    if(new URL(request.url).pathname==='/snapshot') {
      const body=await request.clone().json();
      if(body.machine_id) await this.env.MODEL.getByName('startup').fetch('https://fixture.internal/origin-selected?machine='+encodeURIComponent(body.machine_id));
      if(!body.machine_id) await this.env.MODEL.getByName('startup').fetch('https://fixture.internal/origin-inventory');
    }
    return super.fetch(request);
  }
}
export class UserAccount extends RealUserAccount {
  constructor(state,env) { super(state,env); this.fixture=env.MODEL; }
  async fetch(request) {
    if(request.method==='GET' && new URL(request.url).pathname.startsWith('/api-keys/')) {
      await this.fixture.getByName('startup').fetch('https://fixture.internal/key-lookup');
    }
    if(request.method==='POST' && new URL(request.url).pathname.endsWith('/prepare')) {
      await this.fixture.getByName('startup').fetch('https://fixture.internal/registry-prepare');
    }
    if(request.method==='POST' && new URL(request.url).pathname.endsWith('/publish')) {
      const model=this.fixture.getByName('startup');
      const gate=await model.fetch('https://fixture.internal/publication');
      if(!gate.ok) return gate;
      const response=await super.fetch(request);
      if(response.ok) await model.fetch('https://fixture.internal/published');
      return response;
    }
    return super.fetch(request);
  }
}
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record && typeof record==='object'?JSON.stringify(record):record,...rest);
export class FixtureEgress extends WorkerEntrypoint {
  // Exercise rollout fallback; the egress journey covers acknowledged handoff.
  prepareModelUpgrade() { return { status: "unsupported" }; }
  fetch(request) { return this.env.MODEL.getByName('startup').fetch(request); }
  async readAccountDiscovery(owner,component) {
    const response=await this.env.MODEL.getByName('startup').fetch('https://fixture.internal/'+component);
    return {status:response.status,schema:1,expiresAt:Date.now()+900000,data:await response.json()};
  }
}
// No sandbox is allocated in this journey; cleanup still visits the external
// container namespace to remove any possible retained legacy resource.
export class FixtureSandbox extends DurableObject {
  async clearRemoteDesktop() {}
  async destroy() {}
}
const codeCall = (name, callId, args) => ({type:'custom_tool_call',name:'exec',call_id:callId,input:'text(await tools.'+name+'('+JSON.stringify(args)+'));'});
export class FixturePrewarm extends WorkerEntrypoint {
  async prewarm(input) {
    await this.env.MODEL.getByName('startup').fetch('https://fixture.internal/prewarm?owner='+input.owner+'&region='+input.region);
    return { outcome: 'unavailable' };
  }
}
export class FixtureModel extends DurableObject {
  originInventoryReleased=false; releaseOriginInventory; selectedReleased=false; releaseSelected;
  voiceHoldSent=false; voiceEnvironmentSent=false; voiceNewEnvironmentSent=false; originEnvironmentSent=false; walletEnvironmentSent=false; releaseVoice;
  holdPublication=true; releaseRegistration;
  walletEnabled=false; releaseWallet; holdVault=true; releaseVault; vaultReady=false; holdSetup=true; releaseSetup;
  prewarmReleased=false; prewarmWaiters=[];
  events=[]; setupStarted=false; catalogStarted=false; catalogReleased=false; setupFinished=false; published=false; publicationAttempts=0; holdCatalog=true; catalogGate; release; releasePublication;
  record(event,extra={}) { const row={type:'fixture.startup',event,at:Date.now(),...extra};this.events.push(row);console.info(row); }
  async fetch(request) {
    const url=new URL(request.url);
    if(url.pathname==='/origin-selected') { const machine=url.searchParams.get('machine');this.record('origin.selected',{machine});if(machine==='stalled-origin-hand' && !this.selectedReleased) await new Promise(resolve=>{this.releaseSelected=resolve;});this.record('origin.selected.released',{machine});return new Response(null,{status:204}); }
    if(url.pathname==='/release-selected') {this.selectedReleased=true;this.releaseSelected?.();this.originEnvironmentSent=false;return new Response(null,{status:204});}
    if(url.pathname==='/hold-origin-inventory') {this.originInventoryReleased=false;return new Response(null,{status:204});}
    if(url.pathname==='/origin-inventory') { if(!this.originInventoryReleased) await new Promise(resolve=>{this.releaseOriginInventory=resolve;}); return new Response(null,{status:204}); }
    if(url.pathname==='/release-origin-inventory') {this.originInventoryReleased=true;this.releaseOriginInventory?.();return new Response(null,{status:204});}
    if(url.pathname==='/prewarm') {this.record('credential.prewarm',{owner:url.searchParams.get('owner'),region:url.searchParams.get('region')});if(!this.prewarmReleased) await new Promise(resolve=>this.prewarmWaiters.push(resolve));return new Response(null,{status:204});}
    if(url.pathname==='/release-prewarm') {this.prewarmReleased=true;for(const resolve of this.prewarmWaiters)resolve();return new Response(null,{status:204});}
    if(url.pathname==='/trace') return Response.json(this.events);
    if(url.pathname==='/release-voice') { this.releaseVoice?.();return new Response(null,{status:204}); }
    if(url.pathname==='/key-lookup') { this.record('key.lookup');return new Response(null,{status:204}); }
    if(url.pathname==='/allow-wallet') { this.walletEnabled=true;this.releaseWallet?.();return new Response(null,{status:204}); }
    if(url.pathname==='/registry-prepare') { this.record('registry.prepare');return new Response(null,{status:204}); }
    if(url.pathname==='/release-publication') { this.holdPublication=false;this.releaseRegistration?.();return new Response(null,{status:204}); }
    if(url.pathname==='/publication') {
      this.record('publication.start');
      if(this.holdPublication) { this.record('publication.held');await new Promise(resolve=>{this.releaseRegistration=resolve;}); }
      if(!this.catalogStarted) await new Promise(resolve=>{this.releasePublication=resolve;setTimeout(resolve,3000);});
      this.record('publication.catalog_observed',{observed:this.catalogStarted});
      if(++this.publicationAttempts===1) { this.record('publication.fail');return new Response(null,{status:503}); }
      return new Response(null,{status:204});
    }
    if(url.pathname==='/published') { this.published=true;this.record('publication.committed');return new Response(null,{status:204}); }
    if(url.pathname==='/hold-catalog') { this.holdCatalog=true;this.catalogReleased=false;return new Response(null,{status:204}); }
    if(url.pathname==='/hold-vault') { this.holdVault=true;this.vaultReady=false;return new Response(null,{status:204}); }
    if(url.pathname==='/release-vault') { this.holdVault=false;this.releaseVault?.();return new Response(null,{status:204}); }
    if(url.pathname==='/release-setup') { this.holdSetup=false;this.releaseSetup?.();return new Response(null,{status:204}); }
    if(url.pathname==='/release-catalog') { this.holdCatalog=false;this.catalogGate?.();return new Response(null,{status:204}); }
    if(url.pathname==='/catalog') {
      this.catalogStarted=true;this.record('catalog.start');this.releasePublication?.();
      if(this.holdCatalog) { this.record('catalog.held');await new Promise(resolve=>{this.catalogGate=resolve;}); }
      if(!this.setupStarted) await new Promise(resolve=>{this.release=resolve;setTimeout(resolve,3000);});
      this.record('catalog.setup_observed',{observed:this.setupStarted});
      await new Promise(resolve=>setTimeout(resolve,150));
      this.catalogReleased=true; this.record('catalog.finish');
      return Response.json({connectors:{},mcp_connections:[]});
    }
    if(url.pathname==='/vault') { this.record('vault.read');if(this.holdVault) { this.record('vault.held');await new Promise(resolve=>{this.releaseVault=resolve;}); } this.vaultReady=true;this.record('vault.finish');return Response.json([]); }
    if(url.pathname.endsWith('/wallet')) {
      this.record('wallet.read',{combined:request.headers.get('accept')==='application/vnd.nanocodex.wallet-snapshot+json'});
      if(!this.walletEnabled) await new Promise(resolve=>{this.releaseWallet=resolve;});
      return Response.json({address:'0x'+'1'.repeat(40),created_at:1,balance:{account:'0x'+'1'.repeat(40),balance:'12345678',decimals:6,symbol:'MACH',token:'0x20c000000000000000000000f37de3740adec032'}});
    }
    if(url.pathname.endsWith('/wallet/balance')) { this.record('wallet.redundant_balance_read');return new Response(null,{status:503}); }
    if(request.headers.get('x-nanocodex-target-url')==='https://startup-fixture.example/setup') {
      this.setupStarted=true;this.record('setup.start',{published:this.published});this.release?.();
      if(this.holdSetup) { this.record('setup.held');await new Promise(resolve=>{this.releaseSetup=resolve;}); }
      this.setupFinished=true;this.record('setup.finish');return new Response('SETUP_OK');
    }
    if(request.headers.get('x-nanocodex-target-url')==='https://startup-fixture.example/voice-hold') {
      this.record('voice.tool.held');
      await new Promise(resolve=>{this.releaseVoice=resolve;});
      return new Response('VOICE_RELEASED');
    }
    if(request.headers.get('upgrade')==='websocket') {
      this.record('provider.connect',{published:this.published,session:request.headers.get('session-id'),headers:Object.fromEntries(request.headers)});
      const [client,server]=Object.values(new WebSocketPair());server.accept();
      server.addEventListener('close',()=>server.close(1000));
      let effectiveTools=[],requestIndex=0;
      server.addEventListener('message',event=>{
        const body=JSON.parse(event.data);
        const definitions=[...(body.tools??[]),...(body.input??[]).filter(item=>item.type==='additional_tools').flatMap(item=>item.tools??[])];
        if(definitions.length) effectiveTools=definitions.map(tool=>tool.name??tool.function?.name);
        const id='resp_'+crypto.randomUUID();
        this.record('provider.request',{response_id:id,previous_response_id:body.previous_response_id,catalog_ready:this.catalogReleased,vault_ready:this.vaultReady,setup_ready:this.setupFinished,
          tools:effectiveTools,input:body.input,reasoning:body.reasoning,service_tier:body.service_tier});
        ++requestIndex;
        const inputText=JSON.stringify(body.input??[]);
        if(inputText.includes('VOICE_ORIGIN_HOLD') && !this.voiceHoldSent) {
          this.voiceHoldSent=true;
          server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:false,output:[codeCall('exec_command','call_voice_hold',{cmd:'curl -fsS https://startup-fixture.example/voice-hold',workdir:'/brain'})],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
          return;
        }
        const voiceEnvironment=inputText.includes('VOICE_ORIGIN_STEER') && !this.voiceEnvironmentSent;
        const voiceNewEnvironment=inputText.includes('VOICE_ORIGIN_NEW') && !this.voiceNewEnvironmentSent;
        if(voiceEnvironment || voiceNewEnvironment) {
          if(voiceEnvironment) this.voiceEnvironmentSent=true;
          if(voiceNewEnvironment) this.voiceNewEnvironmentSent=true;
          server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:false,output:[{type:'custom_tool_call',name:'exec',call_id:voiceEnvironment?'call_voice_origin':'call_voice_new_origin',input:'text(await tools.environment({}));'}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
          return;
        }
        if(inputText.includes('Inspect current environment origin') && !this.originEnvironmentSent) {
          this.originEnvironmentSent=true;
          server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:false,output:[{type:'custom_tool_call',name:'exec',call_id:'call_current_origin',input:'text(await tools.environment({}));'}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
          return;
        }
        if(requestIndex===2) {
          server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:false,output:[codeCall('exec_command','call_startup_read',{cmd:'cat /brain/setup-output.txt',workdir:'/brain'})],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
          return;
        }
        if(inputText.includes('Read the account wallet with environment') && !this.walletEnvironmentSent) {
          this.walletEnvironmentSent=true;
          server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:false,output:[{type:'custom_tool_call',name:'exec',call_id:'call_wallet_environment',input:'text(await tools.environment({}));'}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
          return;
        }
        server.send(JSON.stringify({type:'response.created',response:{id,status:'in_progress'}}));
        server.send(JSON.stringify({type:'response.output_text.delta',output_index:0,delta:'STARTUP_OK'}));
        server.send(JSON.stringify({type:'response.completed',response:{id,status:'completed',end_turn:true,
          output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'STARTUP_OK'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
      });
      return new Response(null,{status:101,webSocket:client});
    }
    return Response.json({tools:[],machines:[],connections:[],status:'not_configured'});
  }
}
export default {async fetch(request,env,ctx) {
  const url=new URL(request.url);
  if(url.pathname==='/__activation-probe') return Response.json(await env.NANOCODEX_SESSIONS.getByName('fresh-activation-probe').activationProbe());
  if(url.pathname==='/__origin-state') return env.NANOCODEX_SESSIONS.getByName(url.searchParams.get('agent')).fetch('https://session.internal/fixture-origin-state?turn='+url.searchParams.get('turn'));
  if(url.pathname==='/__fixture-hand') return env.NANOCODEX_ACCOUNT_TOOLS.getByName(url.searchParams.get('owner')).fetch(new Request('https://account-tools.internal/tool-host',request));
  if(env.EDGE) {
    if(request.headers.get('x-fixture-direct-run')==='required') env={...env,NANOCODEX_BACKEND:{fetch(){throw Error('managed Worker hop is held');}}};
    Object.defineProperty(request,'cf',{value:{colo:'SFO'}});
    return await routeManaged(request,env,url,ctx)??new Response(null,{status:404});
  }
  if(url.pathname==='/__fixture') {
    const {user}=await request.json();await ensureAccount(env,user,true);
    const auth=await (await env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/authorization')).json();
    return Response.json({user,...await createApiKey(env,{kind:'api_key',userId:user,...auth.grant,
      subjectId:'api_key:'+user,credentialId:'fixture',capabilities:auth.grant.capabilities},'synthetic startup')});
  }
  if(url.pathname==='/__revoke-key') {const {user,id}=await request.json();return Response.json(await revokeApiKey(env,user,id));}
  if(url.pathname==='/__release-selected' || url.pathname==='/__hold-origin-inventory') return env.MODEL.getByName('startup').fetch('https://fixture.internal/'+url.pathname.slice(3));
  if(url.pathname==='/__release-origin-inventory') return env.MODEL.getByName('startup').fetch('https://fixture.internal/release-origin-inventory');
  if(url.pathname==='/__release-publication') return env.MODEL.getByName('startup').fetch('https://fixture.internal/release-publication');
  if(url.pathname==='/__trace') return env.MODEL.getByName('startup').fetch('https://fixture.internal/trace');
  if(url.pathname==='/__release-prewarm') return env.MODEL.getByName('startup').fetch('https://fixture.internal/release-prewarm');
  if(url.pathname==='/__release-voice') return env.MODEL.getByName('startup').fetch('https://fixture.internal/release-voice');
  if(url.pathname==='/__hold-vault' || url.pathname==='/__release-vault' || url.pathname==='/__release-setup' || url.pathname==='/__hold-catalog' || url.pathname==='/__release-catalog' || url.pathname==='/__allow-wallet') return env.MODEL.getByName('startup').fetch('https://fixture.internal/'+url.pathname.slice(3));
  return worker.fetch(request,env,ctx);
}};
`;

const originOnly = process.env.NANOCODEX_STARTUP_ORIGIN_ONLY === "1";
test(originOnly ? "cold authorized Hand origin and admission replay through account HTTP and WebSocket" : "public HTTP and WebSocket startup omit wallet I/O while explicit environment remains live", { timeout: 90_000 }, async () => {
  await mkdir(output, { recursive: true });
  const runtime = [], records = [], http = [];
  const capture = line => { runtime.push(line); const offset=line.indexOf('{"type":');if(offset>=0)try{records.push(JSON.parse(line.slice(offset)));}catch{} };
  const assets=[];
  const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,metafile:true,nodePaths:[join(candidateRoot,"node_modules"),join(candidateRoot,"../nanocodex/node_modules"),join(candidateRoot,"../../node_modules")],format:"esm",platform:"node",conditions:["workerd"],target:"es2022",
    banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:["cloudflare:*","node:*"],
    alias:{"node-rsa":join(root,"../nanocodex/tools/browser/unsupportedNodeRsa.mjs")},plugins:[{name:"baseline-generated-artifacts",setup(builder){
      // The comparison checkout supplies production source; generated runtime
      // artifacts and installed packages come from this prepared test checkout.
      if(root!==candidateRoot) builder.onResolve({filter:/^\.\/just-bash-lazy\.mjs$/},()=>({path:join(candidateRoot,"src/just-bash-lazy.mjs")}));
    }},{name:"wasm",setup(builder){builder.onResolve({filter:/\.wasm$/},async args=>{
      const contents=await readFile(root!==candidateRoot && args.path==="./quickjs.wasm" ? join(candidateRoot,"src/quickjs.wasm") : join(args.resolveDir,args.path)),name=`fixture-${assets.length}.wasm`;assets.push({type:"CompiledWasm",path:name,contents});return {path:`./${name}`,external:true};
    });}}],logLevel:"silent"});
  const modules=[{type:"ESModule",path:"worker.mjs",contents:bundle.outputFiles[0].text},...assets];
  const common={modules,compatibilityDate:"2026-07-30",compatibilityFlags:["nodejs_compat","enable_request_signal"]};
  const mf=new Miniflare({port:0,handleRuntimeStdio(stdout,stderr){createInterface({input:stdout}).on("line",capture);createInterface({input:stderr}).on("line",capture);},
    durableObjectsPersist:join(output,"sqlite"),r2Persist:join(output,"r2"),workers:[
      {...common,name:"edge",bindings:{EDGE:true},serviceBindings:{NANOCODEX_BACKEND:"managed"},
        durableObjects:{NANOCODEX_LIVE_API_KEYS:{className:"ApiKeyRecord",scriptName:"managed",useSQLite:true},
          NANOCODEX_LIVE_SESSIONS:{className:"OriginAgentSession",scriptName:"managed",useSQLite:true}}},
      {...common,name:"managed",bindings:{NANOCODEX_PERFORMANCE_TRACE:"true",MANAGED_AGENT_DIRECT_CREDENTIALS:"true",AGENT_IDLE_TIMEOUT_MS:"60000"},
        durableObjects:{NANOCODEX_SESSIONS:{className:"OriginAgentSession",useSQLite:true},NANOCODEX_USERS:{className:"UserAccount",useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:"Organization",useSQLite:true},
          NANOCODEX_API_KEYS:{className:"ApiKeyRecord",useSQLite:true},NANOCODEX_AUTH:{className:"NonceStorage",useSQLite:true},NANOCODEX_ACCOUNT_TOOLS:{className:"OriginAccountHostedTools",useSQLite:true},
          MODEL:{className:"FixtureModel",useSQLite:true},NANOCODEX_MEMORY:{className:"FixtureModel",useSQLite:true},NANOCODEX_SANDBOXES:{className:"FixtureSandbox",useSQLite:true}},
        serviceBindings:{NANOCODEX:{name:"managed",entrypoint:"FixtureEgress"},NANOCODEX_SESSION_MODEL_EGRESS:{name:"managed",entrypoint:"FixtureEgress"}},r2Buckets:["NANOCODEX_HISTORY","NANOCODEX_WORKSPACES"]},
    ]});
  let failure, live, handAttachment, foreignAttachment, stalledAttachment, evidence={};
  try {
    const base=await mf.ready,backend=await mf.getWorker("managed");
    const fixture=async()=>{const response=await backend.fetch("https://fixture.internal/__fixture",{method:"POST",body:JSON.stringify({user:crypto.randomUUID()})});assert.equal(response.status,200);return response.json();};
    const activationResponse=await backend.fetch("https://fixture.internal/__activation-probe");
    assert.equal(activationResponse.status,200,"fresh activation probe works before schema admission");
    const activation=await activationResponse.json();
    assert.ok(activation.handler_entered_at_ms>=activation.constructor_entered_at_ms);
    evidence.activation_probe=activation;
    const {token,user}=await fixture(),foreign=await fixture(),other=foreign.token;
    const handId="synthetic-origin-hand";
    const handTools=await createTools({tools:{}});
    const attachHand=async(owner,id)=>{
      const attachment=createAttachment(handTools,{endpoint:"wss://fixture.internal/tool-host",transport:{async connect(){
        const response=await backend.fetch("https://fixture.internal/__fixture-hand?owner="+owner,{headers:{upgrade:"websocket","x-nanocodex-owner-id":owner}});
        const socket=response.webSocket; socket.accept(); return socket;
      }}},{machines:[{id,name:"Synthetic Origin Hand",workspace:"/fixture",capabilities:["shell"]}],attachmentId:id});
      assert.equal((await attachment.connect()).connected,true); return attachment;
    };
    handAttachment=await attachHand(user,handId);
    foreignAttachment=await attachHand(foreign.user,"foreign-origin-hand");
    const call=async(path,method="GET",body,expected=200,credential=token,extra={})=>{
      const started=performance.now(),response=await fetch(new URL(path,base),{method,headers:{authorization:"Bearer "+credential,"content-type":"application/json",...extra},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(20000)});
      const raw=await response.text(),value=raw?JSON.parse(raw):null;http.push({path,method,status:response.status,elapsed_ms:performance.now()-started,value});assert.equal(response.status,expected,JSON.stringify(value));return value;
    };
    const settings={model:"gpt-6.1-sol",thinking:"low",reasoning_mode:"standard",fast_mode:false};
    const configuration={environment:{files:[{path:"/brain/setup-input.txt",content:"durable fixture"}],skills:[],setup_commands:["curl -fsS https://startup-fixture.example/setup > /brain/setup-output.txt"],network:{access:"enabled"}}};
    await call("/v1/agent-runs","POST",{input:null,settings},400);
    assert.equal((await (await backend.fetch("https://fixture.internal/__trace")).json()).length,0,"invalid input starts no metadata or model work");
    const started=performance.now();
    const run=await call("/v1/agent-runs","POST",{input:"Reply STARTUP_OK",settings,configuration},201,token,{"idempotency-key":"startup-overlap", "x-nanocodex-client-context":JSON.stringify({client:"nanocodex2",timezone:"Europe/Athens",hand:"user:"+handId,native_cwd:"/fixture/project"})});
    const waitTurn=async (id,agentId=run.agent_id,credential=token)=>{
      for(let i=0;i<1000;i++){const value=await call(`/v1/agents/${agentId}/turns/${id}`,"GET",undefined,200,credential);assert.ok(!["failed","cancelled"].includes(value.state),JSON.stringify(value));if(value.state==="completed")return value;await delay(10);}
      throw Error("turn did not finish");
    };
    const cold=await waitTurn(run.turn_id),coldMs=performance.now()-started;assert.match(JSON.stringify(cold),/STARTUP_OK/);
    const coldTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    assert.equal(coldTrace.some(row=>row.event==="publication.committed"),false,"first answer precedes held registry publication");
    await backend.fetch("https://fixture.internal/__release-origin-inventory");
    await backend.fetch("https://fixture.internal/__release-publication");
    for(let i=0;;i++) {
      const trace=await(await backend.fetch("https://fixture.internal/__trace")).json();
      if(trace.some(row=>row.event==="publication.committed"))break;
      assert.ok(i<500,"background registry publication did not retry");await delay(10);
    }
    const listed=await call("/v1/agents");
    assert.ok(listed.data.includes(run.agent_id),"completed thread remains discoverable");
    assert.equal(coldTrace.filter(row=>row.event==="provider.request").length,1);
    for(const event of ["catalog.finish","vault.finish","setup.start","setup.finish"]) assert.equal(coldTrace.some(row=>row.event===event),false,"first answer precedes "+event);
    await call(`/v1/agents/${run.agent_id}/turns/${run.turn_id}`,"GET",undefined,404,other);
    const warmInput={id:crypto.randomUUID(),input:"Read the prepared file and reply STARTUP_OK"};
    const originHeaders=client=>client?{"x-nanocodex-client-context":JSON.stringify({client,timezone:"UTC"})}:{};
    const warmStarted=performance.now(),warm=await call(`/v1/agents/${run.agent_id}/turns`,"POST",warmInput,202,token,originHeaders("iphone"));
    const warmRetry=await call(`/v1/agents/${run.agent_id}/turns`,"POST",warmInput,200,token,originHeaders("different-retry-device"));
    assert.equal(warmRetry.turn_id,warm.turn_id);
    // Wait for the real SDK to dispatch exec, then verify its continuation is
    // blocked on setup. Optional metadata remains held throughout the read.
    for(let i=0;;i++) {
      const pending=await(await backend.fetch("https://fixture.internal/__trace")).json();
      if(pending.filter(row=>row.event==="provider.request").length===2 && pending.some(row=>row.event==="setup.held"))break;
      assert.ok(i<500,"first-use exec was not requested while setup was held");await delay(10);
    }
    const heldTurn=await call(`/v1/agents/${run.agent_id}/turns/${warm.turn_id}`);
    assert.notEqual(heldTurn.state,"completed","first exec cannot read configured output before setup completes");
    await delay(100);
    assert.equal((await(await backend.fetch("https://fixture.internal/__trace")).json()).filter(row=>row.event==="provider.request").length,2,"no tool continuation before setup release");
    await backend.fetch("https://fixture.internal/__release-setup");
    assert.match(JSON.stringify(await waitTurn(warm.turn_id)),/STARTUP_OK/);
    const afterExec=await(await backend.fetch("https://fixture.internal/__trace")).json();
    for(const event of ["catalog.finish","vault.finish"]) assert.equal(afterExec.some(row=>row.event===event),false,"exec does not join optional "+event);
    await backend.fetch("https://fixture.internal/__release-catalog");
    await backend.fetch("https://fixture.internal/__release-vault");
    for(let i=0;;i++) {
      const finished=await(await backend.fetch("https://fixture.internal/__trace")).json();
      if(finished.some(row=>row.event==="catalog.finish") && finished.some(row=>row.event==="vault.finish"))break;
      assert.ok(i<500,"released discovery did not finish");await delay(10);
    }
    const environment=await call(`/v1/agents/${run.agent_id}/environment`);assert.equal(environment.state,"ready");
    const warmMs=performance.now()-warmStarted,trace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    evidence={source_root:root,cold_public_completion_ms:coldMs,warm_public_completion_ms:warmMs,trace};
    const first=event=>trace.find(row=>row.event===event);
    assert.equal(trace.filter(row=>row.event==="registry.prepare").length,0,"fused creation publishes directly without an unused registry preparation request");
    assert.equal(first("publication.catalog_observed")?.observed,true,"metadata read starts while registration is still pending");
    assert.equal(trace.filter(row=>row.event==="publication.fail").length,1,"fault injector fails the first publication before commit");
    assert.equal(trace.filter(row=>row.event==="publication.start").length,2,"background publication retries without resubmitting the prompt");
    assert.equal(first("setup.start").published,true,"no configured side effect before committed registration");
    assert.equal(first("provider.connect").published,false,"provider connects while optional registration is held");
    assert.equal(first("catalog.setup_observed")?.observed,true,"configured setup must run while catalog is pending");
    assert.ok(first("setup.start").at<first("catalog.finish").at);
    const requests=trace.filter(row=>row.event==="provider.request");assert.equal(requests.length,3);
    for(const request of requests){assert.equal(request.catalog_ready,false);assert.equal(request.vault_ready,false);assert.ok(request.tools.includes("exec"),JSON.stringify(request.tools));}
    assert.equal(requests[0].setup_ready,false);assert.equal(requests[1].setup_ready,false);assert.equal(requests[2].setup_ready,true);
    assert.match(JSON.stringify(requests[0].input),/startup_context/);
    const startupText=requests[0].input.filter(item=>item.role==='developer').flatMap(item=>item.content??[]).map(item=>item.text??'').join('\n');
    const startupEnvironment=JSON.parse(startupText.match(/<environment>\s*([\s\S]*?)\s*<\/environment>/)[1].replaceAll('&lt;','<').replaceAll('&gt;','>').replaceAll('&amp;','&'));
    assert.equal(Object.hasOwn(startupEnvironment,"wallet"),false,"startup leaves unrequested wallet metadata unknown");
    assert.deepEqual(startupEnvironment.identity,{});
    assert.deepEqual(startupEnvironment.stablecoins,[]);
    assert.equal(trace.filter(row=>row.event==="wallet.read").length,0,"model startup never requests a wallet snapshot");
    assert.equal(trace.filter(row=>row.event==="wallet.redundant_balance_read").length,0,"no sequential balance fetch");
    assert.match(JSON.stringify(requests[2].input),/SETUP_OK/,"real shell reads the R2 file created by setup");
    assert.equal(trace.filter(row=>row.event==="catalog.start").length,1,"warm turn reuses bounded discovery");
    assert.equal(trace.filter(row=>row.event==="setup.start").length,1,"warm turn never repeats setup side effects");
    evidence={source_root:root,cold_public_completion_ms:coldMs,warm_public_completion_ms:warmMs,setup_catalog_overlap_ms:first("catalog.finish").at-first("setup.start").at,
      setup_once:true,catalog_reads:1,provider_requests:3,prepared_file_read:true,tools_preserved:true,cross_owner_denied:true,
      discovery_before_registration:true,failed_publication_retried:true,first_answer_before_registration:true,registry_prepare_requests:0,startup_wallet_reads:0,trace};
    // Exercise native create-on-upgrade with speculative preparation while
    // the external wallet service is held. No wallet read may even start.
    const liveToken=(await fixture()).token;
    await backend.fetch('https://fixture.internal/__hold-catalog');
    await backend.fetch('https://fixture.internal/__hold-vault');
    const liveUrl=new URL('/v1/agents/live',base);liveUrl.protocol='ws:';
    for(const [key,value] of Object.entries(settings))liveUrl.searchParams.set(key,String(value));
    const wire=[];let socketError,upgradeStatus;
    live=new WebSocket(liveUrl,{headers:{authorization:'Bearer '+liveToken,'x-nanocodex-prepare':'active-conversation','x-nanocodex-api-key-object-id':'a'.repeat(64)}});
    live.on('upgrade',response=>{upgradeStatus=response.statusCode;});
    live.on('message',data=>wire.push(JSON.parse(String(data))));live.on('error',error=>{socketError=error;});
    const waitMessage=async predicate=>{
      for(let i=0;i<2000;i++){if(socketError)throw socketError;const message=wire.find(predicate);if(message)return message;await delay(10);}
      throw Error('WebSocket startup did not complete: '+JSON.stringify(wire));
    };
    const ready=await waitMessage(message=>message.type==='ready'),liveTurn=crypto.randomUUID();
    assert.equal(upgradeStatus,101,'prepared live request upgrades while fresh discovery is held');
    const admissionRace=records.find(row=>row.type==='fixture.admission_race');
    assert.deepEqual(admissionRace?.statuses,[409,409,409],'live, fused and standalone admissions lose to the reserved create');
    assert.equal(admissionRace.winner,101);
    evidence.admission_race=admissionRace;
    const admitted=await call(`/v1/agents/${ready.session_id}`,"GET",undefined,200,liveToken);
    assert.equal(admitted.session_id,ready.session_id,'winner retains the public session identity');
    await call(`/v1/agents/${ready.session_id}`,"GET",undefined,404,other);
    const warming=await (await backend.fetch('https://fixture.internal/__trace')).json();
    assert.equal(warming.filter(row=>row.event==='provider.request').length,3,'auth-only live preparation sends no provider frames');
    live.send(JSON.stringify({type:'prompt',id:liveTurn,input:'Reply STARTUP_OK'}));
    await waitMessage(message=>message.type==='turn_accepted' && message.id===liveTurn);
    // A fresh owner's discovery is withheld at the real service boundary.
    // Ready and prompt acceptance must remain independent of metadata readiness.
    try {
      for(let i=0;;i++) {
        const pending=await(await backend.fetch('https://fixture.internal/__trace')).json();
        if(pending.filter(row=>row.event==='catalog.held').length===2 && pending.filter(row=>row.event==='vault.read').length===2) {
          assert.equal(pending.filter(row=>row.event==='catalog.finish').length,1,'fresh live catalog remains held after ready and prompt acceptance');
          assert.match(JSON.stringify(await waitTurn(liveTurn,ready.session_id,liveToken)),/STARTUP_OK/,'first live answer precedes discovery release');
          break;
        }
        assert.ok(i<200,'prepared live discovery did not dispatch both components');await delay(10);
      }
    } finally { await backend.fetch('https://fixture.internal/__release-catalog');await backend.fetch('https://fixture.internal/__release-vault'); }
    assert.match(JSON.stringify(await waitTurn(liveTurn,ready.session_id,liveToken)),/STARTUP_OK/);
    const liveTrace=await(await backend.fetch('https://fixture.internal/__trace')).json();
    const liveRequests=liveTrace.filter(row=>row.event==='provider.request');
    assert.equal(liveTrace.filter(row=>row.event==='provider.connect').length,2,'live turn opens exactly one fallback socket');
    assert.ok(records.some(row=>row.type==='managed.model_upgrade_preparation' && row.outcome==='ack_unavailable'),'unsupported preparation safely falls back');
    assert.equal(liveRequests.length,4);
    assert.ok(liveRequests[3].tools.includes('exec'),'first live prompt retains tools after discovery');
    assert.match(JSON.stringify(liveRequests[3].input),/startup_context/,'first live prompt retains the startup snapshot');
    assert.equal(liveRequests[3].catalog_ready,false,'live first request precedes catalog release');
    assert.equal(liveRequests[3].vault_ready,false,'live first request precedes Vault release');
    assert.ok(liveTrace.findIndex(row=>row===liveRequests[3])>liveTrace.findIndex(row=>row.event==='catalog.held'),'first live request occurs while discovery is held');
    assert.equal(liveTrace.filter(row=>row.event==='catalog.start').length,2,'prepared live prompt reuses its discovery read');
    assert.equal(liveTrace.filter(row=>row.event==='vault.read').length,2,'prepared live prompt reuses its Vault metadata read');
    assert.equal(liveTrace.filter(row=>row.event==='key.lookup').length,0,'verified key route avoids the account locator hop');
    assert.equal(liveTrace.filter(row=>row.event==='wallet.read').length,0,'HTTP and prepared WebSocket startup perform no wallet I/O');
    live.close();

    // An explicit environment tool call still retrieves the live snapshot.
    await backend.fetch('https://fixture.internal/__allow-wallet');
    const inspected=await call(`/v1/agents/${run.agent_id}/turns`,'POST',{id:crypto.randomUUID(),input:'Read the account wallet with environment and reply STARTUP_OK'},202);
    assert.match(JSON.stringify(await waitTurn(inspected.turn_id)),/STARTUP_OK/);
    const inspectedTrace=await(await backend.fetch('https://fixture.internal/__trace')).json();
    assert.equal(inspectedTrace.filter(row=>row.event==='wallet.read').length,1,'only explicit environment reads the wallet');
    assert.equal(inspectedTrace.find(row=>row.event==='wallet.read').combined,true);
    const inspectedRequests=inspectedTrace.filter(row=>row.event==='provider.request');
    assert.equal(inspectedRequests.length,6);
    assert.match(JSON.stringify(inspectedRequests.at(-1).input),/12345678/,'live environment result reaches the model');
    evidence={...evidence,websocket_startup_wallet_reads:0,websocket_key_locator_reads:0,websocket_upgrade_status:upgradeStatus,websocket_ready_while_discovery_held:true,websocket_first_tools_preserved:true,websocket_discovery_reads_coalesced:true,forged_key_route_overwritten:true,explicit_environment_wallet_reads:1,explicit_environment_balance:true,provider_requests:6,wire};
    // Exercise device changes through public HTTP and WebSocket admission. Origin
    // travels in the actual model input; it must not bleed between queued turns.
    const userText=request=>(request.input??[]).filter(item=>item.role==="user")
      .map(item=>(item.content??[]).filter(part=>part.type==="input_text").map(part=>part.text).join("\n")).at(-1)??"";
    const currentOrigins=request=>[...userText(request).matchAll(/<current_request_context>([\s\S]*?)<\/current_request_context>/g)]
      .map(match=>JSON.parse(match[1].match(/<request_origin>\s*([\s\S]*?)\s*<\/request_origin>/)[1]));
    const environmentOutput=(request,callId="call_current_origin")=>{
      const output=request.input.find(item=>item.type==="custom_tool_call_output" && item.call_id===callId);
      assert.ok(output,"environment result reaches the actual provider boundary");
      return output.output.filter(part=>part.type==="input_text").map(part=>{try{return JSON.parse(part.text);}catch{return null;}})
        .find(value=>value?.request_origin);
    };
    assert.equal(currentOrigins(requests[0]).at(-1).client.name,"nanocodex2");
    assert.equal(currentOrigins(requests[0]).at(-1).hand?.key,"user:"+handId,"cold admission resolves the authorized reported Hand while full inventory is held");
    assert.equal(currentOrigins(requests[0]).at(-1).native_cwd,"/fixture/project");
    const firstContext=requests[0].input.flatMap(item=>item.content??[]).map(part=>part.text??"").join("\n");
    const startupBlock=firstContext.match(/<startup_context>([\s\S]*?)<\/startup_context>/)?.[1];
    assert.ok(startupBlock,"startup snapshot reaches provider before environment is called");
    const startupOrigin=JSON.parse(startupBlock.match(/<request_origin>\s*([\s\S]*?)\s*<\/request_origin>/)[1]);
    assert.deepEqual(startupOrigin,currentOrigins(requests[0]).at(-1),"startup and first current origin agree before environment");
    assert.equal(currentOrigins(requests[1]).at(-1).client.name,"iphone","retry from another device cannot replace admitted origin");
    const selectedBefore=(await(await backend.fetch("https://fixture.internal/__trace")).json()).filter(row=>row.event==="origin.selected" && row.machine===handId).length;
    assert.equal(selectedBefore,1,"cold attribution performs exactly one selected lookup");
    const originRequestOffset=inspectedRequests.length;
    const envTurn=await call(`/v1/agents/${run.agent_id}/turns`,"POST",{id:crypto.randomUUID(),input:"Inspect current environment origin"},202,token,{"x-nanocodex-client-context":JSON.stringify({client:"linux-cli",hand:"user:"+handId,native_cwd:"/fixture/project",timezone:"UTC"})});
    await waitTurn(envTurn.turn_id);
    const envTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    assert.equal(envTrace.filter(row=>row.event==="origin.selected" && row.machine===handId).length,selectedBefore,"cached authorized origin adds no selected RPC");
    const envRequests=envTrace.filter(row=>row.event==="provider.request");
    assert.equal(currentOrigins(envRequests[originRequestOffset]).at(-1).client.name,"linux-cli");
    const liveEnvironment=environmentOutput(envRequests[originRequestOffset+1]);
    assert.equal(liveEnvironment.request_origin.client.name,"linux-cli","live environment reports this tool call's turn origin");
    assert.equal(liveEnvironment.request_origin.transport,"http");
    assert.equal(liveEnvironment.execution_preferences.advisory,true);
    assert.equal(liveEnvironment.request_origin.hand.key,"user:"+handId);
    assert.deepEqual(liveEnvironment.request_origin,currentOrigins(envRequests[originRequestOffset]).at(-1));
    const pending=await Promise.all(["web",undefined,"desktop"].map((client,index)=>call(`/v1/agents/${run.agent_id}/turns`,"POST",
      {id:crypto.randomUUID(),input:"QUEUED_ORIGIN_"+index},202,token,client?{"x-nanocodex-client-context":JSON.stringify({client,hand:index===0?"user:unknown-origin-hand":"user:foreign-origin-hand",native_cwd:"/must-not-appear"})}:{})));
    for(const turn of pending) await waitTurn(turn.turn_id);
    const queuedTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const queuedRequests=queuedTrace.filter(row=>row.event==="provider.request").slice(originRequestOffset+2);
    for(let index=0;index<3;index++) {
      const request=queuedRequests.find(row=>userText(row).startsWith("QUEUED_ORIGIN_"+index+"\n"));
      assert.ok(request,"queued request reaches real provider boundary");
      const origin=currentOrigins(request).at(-1);
      assert.equal(origin.client?.name??null,["web",null,"desktop"][index]);
      assert.equal(origin.transport,"http");assert.equal(origin.hand,null);assert.equal(origin.native_cwd,undefined);
    }
    const socket=new WebSocket(new URL(`/v1/agents/${run.agent_id}/ws`,base).href.replace(/^http/,"ws"),
      {headers:{authorization:"Bearer "+token,...originHeaders("terminal-websocket")}});
    try {
      await new Promise((resolve,reject)=>{socket.once("open",resolve);socket.once("error",reject);});
      const id=crypto.randomUUID();
      const accepted=new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>reject(Error("WebSocket prompt admission timed out")),10000);
        const observe=data=>{const frame=JSON.parse(data);if(frame.type==="turn_accepted" && frame.id===id){clearTimeout(timer);socket.off("message",observe);resolve();}};
        socket.on("message",observe);
      });
      socket.send(JSON.stringify({type:"prompt",id,input:"WEBSOCKET_ORIGIN"}));
      await accepted;
      await waitTurn(id);
    } finally { socket.close(); }
    const originTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const originRequests=originTrace.filter(row=>row.event==="provider.request");
    const wsOrigin=currentOrigins(originRequests.at(-1)).at(-1);
    assert.equal(wsOrigin.transport,"websocket");assert.equal(wsOrigin.client.name,"terminal-websocket");
    evidence={...evidence,cold_authorized_hand:true,unknown_and_foreign_hand_rejected:true,per_turn_origin:true,idempotent_origin:true,queued_origin_isolation:true,unknown_caller_cleared:true,websocket_origin:true,live_environment_origin:true};
    // Keep a real typed run inside an admitted tool while two voice requests
    // steer it. Replaying the earlier receipt must not restore its older origin.
    const voice=crypto.randomUUID();
    await call(`/v1/agents/${run.agent_id}/realtime/start`,"POST",{voice_session_id:voice,operation_id:crypto.randomUUID()});
    const held=await call(`/v1/agents/${run.agent_id}/turns`,"POST",{id:crypto.randomUUID(),input:"VOICE_ORIGIN_HOLD"},202,token,originHeaders("desktop-before-voice"));
    for(let i=0;;i++) {
      const heldTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
      if(heldTrace.some(row=>row.event==="voice.tool.held"))break;
      assert.ok(i<200,"voice gate tool did not start");await delay(10);
    }
    const voiceFirst={voice_session_id:voice,operation_id:crypto.randomUUID(),input:"VOICE_ORIGIN_STEER first"};
    const voiceNext={voice_session_id:voice,operation_id:crypto.randomUUID(),input:"VOICE_ORIGIN_STEER latest"};
    try {
      const firstSteer=await call(`/v1/agents/${run.agent_id}/realtime/delegate`,"POST",voiceFirst,202,token,originHeaders("iphone-voice"));
      const nextSteer=await call(`/v1/agents/${run.agent_id}/realtime/delegate`,"POST",voiceNext,202,token,originHeaders("linux-voice"));
      assert.equal(firstSteer.route,"steered");assert.equal(firstSteer.turn_id,held.turn_id);
      assert.equal(nextSteer.route,"steered");assert.equal(nextSteer.turn_id,held.turn_id);
      const replay=await call(`/v1/agents/${run.agent_id}/realtime/delegate`,"POST",voiceFirst,202,token,originHeaders("retry-device"));
      assert.deepEqual(replay,firstSteer);
    } finally { await backend.fetch("https://fixture.internal/__release-voice"); }
    await waitTurn(held.turn_id);
    const steeredTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const steeredRequests=steeredTrace.filter(row=>row.event==="provider.request");
    const voiceResultRequest=steeredRequests.find(row=>row.input?.some(item=>item.call_id==="call_voice_origin" && item.type==="custom_tool_call_output"));
    assert.ok(voiceResultRequest,"voice environment tool completed through actual routing");
    const voiceEnvironment=environmentOutput(voiceResultRequest,"call_voice_origin");
    assert.equal(voiceEnvironment.request_origin.client.name,"linux-voice","older receipt replay cannot overwrite the latest effective origin");
    assert.equal(voiceEnvironment.request_origin.transport,"voice");
    const steeredPrompt=steeredRequests.find(row=>userText(row).startsWith("VOICE_ORIGIN_STEER latest"));
    assert.ok(steeredPrompt,"latest voice steer reaches the actual provider boundary");
    assert.deepEqual(voiceEnvironment.request_origin,currentOrigins(steeredPrompt).at(-1));
    // Idle routing adopts a fresh managed ID; its first environment call must
    // observe that voice origin rather than the preceding steered turn.
    const newVoice=await call(`/v1/agents/${run.agent_id}/realtime/delegate`,"POST",{voice_session_id:voice,operation_id:crypto.randomUUID(),input:"VOICE_ORIGIN_NEW"},202,token,originHeaders("new-voice-device"));
    assert.equal(newVoice.route,"started");assert.notEqual(newVoice.turn_id,held.turn_id);
    await waitTurn(newVoice.turn_id);
    const voiceTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const voiceRequests=voiceTrace.filter(row=>row.event==="provider.request");
    const newVoiceResult=voiceRequests.find(row=>row.input?.some(item=>item.call_id==="call_voice_new_origin" && item.type==="custom_tool_call_output"));
    assert.ok(newVoiceResult);
    assert.equal(environmentOutput(newVoiceResult,"call_voice_new_origin").request_origin.client.name,"new-voice-device");
    await call(`/v1/agents/${run.agent_id}/realtime/stop`,"POST",{voice_session_id:voice,operation_id:crypto.randomUUID()});
    evidence={...evidence,voice_effective_origin:true,voice_origin_receipt_replay:true,voice_adopted_origin:true};
    // A held account-authorized selected lookup must not consume its 10s tool
    // deadline on admission. Keep full inventory gated as well, then release
    // the selected result and inspect actual continued model history.
    const stalledOwner=await fixture();
    stalledAttachment=await attachHand(stalledOwner.user,"stalled-origin-hand");
    await backend.fetch("https://fixture.internal/__hold-origin-inventory");
    const stalledHeaders={"idempotency-key":"stalled-origin","x-nanocodex-client-context":JSON.stringify({client:"nanocodex2",hand:"user:stalled-origin-hand",native_cwd:"/fixture/stalled"})};
    const stalledBody={input:"STALLED_ORIGIN_FIRST",settings};
    const stalledStarted=performance.now();
    const stalledRun=await call("/v1/agent-runs","POST",stalledBody,201,stalledOwner.token,stalledHeaders);
    await waitTurn(stalledRun.turn_id,stalledRun.agent_id,stalledOwner.token);
    const stalledMs=performance.now()-stalledStarted;
    const stalledTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const selectedStart=stalledTrace.find(row=>row.event==="origin.selected" && row.machine==="stalled-origin-hand");
    assert.ok(selectedStart,"stalled case actually enters the selected RPC");
    assert.equal(stalledTrace.some(row=>row.event==="origin.selected.released" && row.machine==="stalled-origin-hand"),false,"first answer precedes selected lookup release");
    const stalledPrompt=stalledTrace.filter(row=>row.event==="provider.request").at(-1);
    assert.ok(stalledPrompt.at-selectedStart.at>=1400,"admission gives lookup its 1.5s budget");
    assert.ok(stalledMs<5000,"stalled selected RPC must not block admission for its 10s deadline: "+stalledMs);
    assert.equal(currentOrigins(stalledPrompt).at(-1).hand,null);
    assert.equal(currentOrigins(stalledPrompt).at(-1).native_cwd,undefined);
    const frozenState=async()=>await(await backend.fetch("https://fixture.internal/__origin-state?agent="+stalledRun.agent_id+"&turn="+stalledRun.turn_id)).json();
    const frozenFirst=await frozenState();
    assert.match(frozenFirst.dispatch,/STALLED_ORIGIN_FIRST/);
    assert.equal(JSON.parse(frozenFirst.startup[0].environment_json).request_origin.hand,null);
    assert.ok(frozenFirst.context.length,"startup context is retained");
    await backend.fetch("https://fixture.internal/__release-selected");
    await backend.fetch("https://fixture.internal/__release-origin-inventory");
    // Drain the authorized response before observing the cached origin on a
    // subsequent admission; the previous enriched prompt must remain frozen.
    await delay(100);
    const stalledReplay=await call("/v1/agent-runs","POST",stalledBody,200,stalledOwner.token,stalledHeaders);
    assert.equal(stalledReplay.turn_id,stalledRun.turn_id);
    const lateTurn=await call(`/v1/agents/${stalledRun.agent_id}/turns`,"POST",{id:crypto.randomUUID(),input:"Inspect current environment origin"},202,stalledOwner.token,{"x-nanocodex-client-context":stalledHeaders["x-nanocodex-client-context"]});
    await waitTurn(lateTurn.turn_id,stalledRun.agent_id,stalledOwner.token);
    const lateTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const latePrompt=lateTrace.filter(row=>row.event==="provider.request").at(-1);
    assert.equal(environmentOutput(latePrompt).request_origin.hand.key,"user:stalled-origin-hand");
    assert.deepEqual(await frozenState(),frozenFirst,"late catalog cannot rewrite retained frozen startup or dispatch prompt");
    assert.equal(lateTrace.filter(row=>row.event==="origin.selected" && row.machine==="stalled-origin-hand").length,1,"replay and cached admission add no selected RPC");
    evidence={...evidence,selected_lookup_budget_ms:1500,stalled_first_answer_ms:stalledMs,late_catalog_preserves_frozen_prompt:true,cached_origin_no_extra_rpc:true};
    if(originOnly) {
      console.log("STARTUP_ORIGIN_EVIDENCE",JSON.stringify({...evidence,trace:undefined,wire:undefined,output}));
      return;
    }

    // Hold a fresh owner's bootstrap after public acceptance, then change
    // defaults. The accepted turn must retain its original inference settings.
    const raceToken=(await fixture()).token;
    const raceSettings={...settings,model:"gpt-6-astra"};
    await backend.fetch("https://fixture.internal/__hold-catalog");
    const racing=await call("/v1/agent-runs","POST",{input:"Reply SETTINGS_PINNED",settings:raceSettings},201,raceToken,{"idempotency-key":"settings-during-bootstrap"});
    let settingsPatch;
    try {
      for(let i=0;;i++) {
        const pendingTrace=await (await backend.fetch("https://fixture.internal/__trace")).json();
        if(pendingTrace.filter(row=>row.event==="catalog.held").length>=3)break;
        assert.ok(i<200,"settings journey discovery did not start");await delay(10);
      }
      settingsPatch=call(`/v1/agents/${racing.agent_id}/settings`,"PATCH",{thinking:"high",fast_mode:true},200,raceToken);
      for(let i=0;;i++) {
        const agent=await call(`/v1/agents/${racing.agent_id}`,"GET",undefined,200,raceToken);
        if(agent.settings?.thinking==="high" && agent.settings?.fast_mode===true)break;
        assert.ok(i<200,"settings PATCH did not update defaults during bootstrap");await delay(10);
      }
    } finally {
      await backend.fetch("https://fixture.internal/__release-catalog");
      if(settingsPatch)await settingsPatch;
    }
    const waitRace=async id=>{
      for(let i=0;i<1000;i++) {
        const turn=await call(`/v1/agents/${racing.agent_id}/turns/${id}`,"GET",undefined,200,raceToken);
        assert.ok(!["failed","cancelled"].includes(turn.state),JSON.stringify(turn));
        if(turn.state==="completed")return turn;await delay(10);
      }
      throw Error("settings journey turn did not finish");
    };
    await waitRace(racing.turn_id);
    const raceTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const pinnedRequest=raceTrace.filter(row=>row.event==="provider.request").at(-1);
    evidence={...evidence,settings_race:{accepted:raceSettings,patched:{thinking:"high",fast_mode:true},first_request:{reasoning:pinnedRequest.reasoning,service_tier:pinnedRequest.service_tier}}};
    assert.equal(pinnedRequest.reasoning?.effort,"low","public acceptance pins reasoning across bootstrap");
    assert.equal(Object.hasOwn(pinnedRequest,"service_tier"),false,"public acceptance pins fast-off and omits service_tier across bootstrap");
    // Astra preserves its initial envelope baseline for the context window.
    // The harness appends trusted effort updates after user input (see Rust
    // supported_reasoning_resume_preserves_pin); assert the actual wire contract.
    const effortUpdates=request=>request.input.filter(item=>item.type==="configuration_update");
    const lowUpdate={type:"configuration_update",reasoning:{effort:"low"}};
    const highUpdate={type:"configuration_update",reasoning:{effort:"high"}};
    assert.deepEqual(effortUpdates(pinnedRequest),[lowUpdate],"accepted effort is the sole initial override despite patched defaults");
    assert.deepEqual(pinnedRequest.input.at(-1),lowUpdate);
    assert.equal(pinnedRequest.input.at(-2).role,"user","initial override follows accepted user input");
    assert.match(JSON.stringify(pinnedRequest.input.at(-2)),/Reply SETTINGS_PINNED/);
    const next=await call(`/v1/agents/${racing.agent_id}/turns`,"POST",{id:crypto.randomUUID(),input:"Reply SETTINGS_UPDATED"},202,raceToken);
    await waitRace(next.turn_id);
    const nextTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const nextRequests=nextTrace.filter(row=>row.event==="provider.request").slice(raceTrace.filter(row=>row.event==="provider.request").length);
    assert.ok(nextRequests.length>1,"updated turn exercises provider sampling and tool continuation");
    const updatedRequest=nextRequests[0];
    evidence.settings_race.next_request={reasoning:updatedRequest.reasoning,service_tier:updatedRequest.service_tier,input:updatedRequest.input};
    assert.deepEqual(effortUpdates(updatedRequest),[lowUpdate,highUpdate],"updated turn retains initial effort and appends exactly one changed override");
    for(const [index,request] of nextRequests.entries()) {
      assert.equal(request.reasoning?.effort,"low","later turn and tool continuation preserve the initial envelope baseline");
      if(index>0) {
        assert.equal(request.previous_response_id,nextRequests[index-1].response_id,"tool continuation retains the response carrying the effective effort");
        assert.deepEqual(effortUpdates(request),[],"WebSocket delta adds no redundant effort override");
        assert.ok(request.input.some(item=>item.type==="custom_tool_call_output"),"continuation submits the tool result");
      }
      assert.equal(request.service_tier,"priority","later turn adopts fast mode update");
    }
    assert.deepEqual(updatedRequest.input.slice(0,pinnedRequest.input.length),pinnedRequest.input,"effort changes preserve the accepted prompt prefix");
    assert.deepEqual(updatedRequest.input.at(-1),highUpdate);
    assert.equal(updatedRequest.input.at(-2).role,"user","changed override follows the new user input");
    assert.match(JSON.stringify(updatedRequest.input.at(-2)),/Reply SETTINGS_UPDATED/);
    const providerCount=nextTrace.filter(row=>row.event==="provider.request").length;
    const replay=await call(`/v1/agents/${racing.agent_id}/turns`,"POST",{id:racing.turn_id,input:"Reply SETTINGS_PINNED"},200,raceToken);
    assert.equal(replay.turn_id,racing.turn_id);
    const replayTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    assert.equal(replayTrace.filter(row=>row.event==="provider.request").length,providerCount,"idempotent retry does not rerun the pinned turn");
    evidence.settings_race.replayed_without_inference=true;
    // A second owner has no L1 snapshot. Its answer completes while metadata
    // remains held. Delete the agent, then release that stale metadata read. The public
    // deletion fence establishes ordering without a sleep-based race assertion.
    await backend.fetch("https://fixture.internal/__hold-catalog");
    const cancelled=await call("/v1/agent-runs","POST",{input:"Reply STARTUP_OK",settings},201,other,{"idempotency-key":"delete-pending-discovery"});
    let deletion,requestsBeforeDeletion;
    try {
      for(let i=0;;i++) {
        const pendingTrace=await (await backend.fetch("https://fixture.internal/__trace")).json();
        if(pendingTrace.filter(row=>row.event==="catalog.held").length>=4)break;
        assert.ok(i<200,"second owner discovery did not start");await delay(10);
      }
      await waitTurn(cancelled.turn_id,cancelled.agent_id,other);
      requestsBeforeDeletion=(await(await backend.fetch("https://fixture.internal/__trace")).json()).filter(row=>row.event==="provider.request").length;
      deletion=call(`/v1/agents/${cancelled.agent_id}`,"DELETE",undefined,204,other);
      let fenced=false;
      for(let i=0;i<200;i++) {
        const response=await fetch(new URL(`/v1/agents/${cancelled.agent_id}`,base),{headers:{authorization:"Bearer "+other}});
        const value=await response.json();
        if(response.status===404 || response.status===409 && value.error==="agent_deleting"){fenced=true;break;}await delay(10);
      }
      assert.equal(fenced,true,"deletion hides the agent before the pending read is released");
    } finally {
      await backend.fetch("https://fixture.internal/__release-catalog");
      if(deletion)await deletion;
    }
    await call(`/v1/agents/${cancelled.agent_id}`,"GET",undefined,404,other);
    const finalTrace=await (await backend.fetch("https://fixture.internal/__trace")).json();
    assert.equal(finalTrace.filter(row=>row.event==="provider.request").length,requestsBeforeDeletion,"late discovery never resurrects a deleted turn");
    evidence={...evidence,invalid_input_no_work:true,deletion_during_discovery_fenced:true,trace:finalTrace};
    // The native POST must work with the intermediate Worker unavailable.
    // Session, live key authorization, SQLite, WASM and provider streaming are real.
    const directOwner=await fixture(), directKey="native-direct-first-turn";
    const directBody={settings,input:"Reply STARTUP_OK"};
    const directFetch=(body=directBody,extra={})=>fetch(new URL("/v1/agent-runs",base),{
      method:"POST",headers:{authorization:"Bearer "+directOwner.token,"content-type":"application/json",
        accept:"text/event-stream","idempotency-key":directKey,"x-fixture-direct-run":"required",
        "x-nanocodex-owner-id":crypto.randomUUID(),"x-nanocodex-api-key-object-id":"a".repeat(64),...extra},
      body:JSON.stringify(body),signal:AbortSignal.timeout(20000)});
    const directResponse=await directFetch();
    const directText=await directResponse.text();
    assert.equal(directResponse.status,201,directText);
    assert.match(directText,/STARTUP_OK/);
    assert.equal(directResponse.headers.has("x-nanocodex-run-phases"),false,"internal timing header is not public");
    const directReceipt=JSON.parse(directText.match(/event: run\ndata: ([^\n]+)/)[1]);
    const directTrace=await(await backend.fetch("https://fixture.internal/__trace")).json();
    const directCount=directTrace.filter(row=>row.event==="provider.request").length;
    await call(`/v1/agents/${directReceipt.agent_id}`,"GET",undefined,404,other);
    const directReplay=await directFetch();
    assert.equal(directReplay.status,200,await directReplay.clone().text());
    assert.match(await directReplay.text(),/STARTUP_OK/);
    const generalReplay=await call("/v1/agent-runs","POST",directBody,200,directOwner.token,{"idempotency-key":directKey});
    assert.equal(generalReplay.agent_id,directReceipt.agent_id,"direct and general routes share creation identity");
    assert.equal(generalReplay.turn_id,directReceipt.turn_id,"direct and general routes share turn identity");
    const conflict=await directFetch({...directBody,input:"Different prompt"});
    assert.equal(conflict.status,409,await conflict.text());
    const invalid=await directFetch({...directBody,input:""},{"idempotency-key":"invalid-native-input"});
    assert.equal(invalid.status,400,await invalid.text());
    const richer=await directFetch({...directBody,configuration:{}});
    assert.equal(richer.status,503,await richer.text(),"richer recipes retain general admission");
    const revokedKey=await backend.fetch("https://fixture.internal/__revoke-key",{method:"POST",body:JSON.stringify({user:directOwner.user,id:directOwner.metadata.id})});
    assert.equal(await revokedKey.json(),true,"fixture revokes the real stored key through the account lifecycle");
    const prewarmTrace=async()=>await (await backend.fetch('https://fixture.internal/__trace')).json();
    const revoked=await directFetch();
    assert.equal(revoked.status,401,await revoked.text(),"direct admission checks live revocation on replay");
    const upgradesBeforeRejected=(await prewarmTrace()).filter(row=>row.event==='provider.connect').length;
    await new Promise((resolve,reject)=>{
      const denied=new WebSocket(liveUrl,{headers:{authorization:'Bearer '+directOwner.token,'x-nanocodex-prepare':'active-conversation'}});
      denied.on('open',()=>{denied.terminate();reject(Error('revoked live authorization unexpectedly upgraded'));});
      denied.on('unexpected-response',(_request,response)=>{response.resume();try{assert.equal(response.statusCode,401);resolve();}catch(error){reject(error);}});
      denied.on('error',reject);
    });
    assert.equal((await prewarmTrace()).filter(row=>row.event==='provider.connect').length,upgradesBeforeRejected,'rejected live authorization starts no speculative provider handshake');
    const afterDirect=await(await backend.fetch("https://fixture.internal/__trace")).json();
    assert.equal(afterDirect.filter(row=>row.event==="provider.request").length,directCount,"replay, conflict, invalid input and revocation start no new inference");
    evidence={...evidence,direct_run_without_managed_hop:true,direct_run_replay:true,direct_run_cross_route_identity:true,direct_run_live_revocation:true};
    console.log("STARTUP_OVERLAP_EVIDENCE",JSON.stringify({...evidence,trace:undefined,wire:undefined,output}));
  } catch(error) {failure=error;throw error;}
  finally {
    live?.terminate();
    await handAttachment?.close();
    await foreignAttachment?.close();
    await stalledAttachment?.close();
    await mf.dispose();
    await Promise.all([writeFile(join(output,"evidence.json"),JSON.stringify({command:(originOnly?"NANOCODEX_STARTUP_ORIGIN_ONLY=1 ":"")+"node --test test/startup-overlap-journey.test.mjs",status:failure?"FAIL":"PASS",error:failure?.stack,...evidence,http,records},null,2)),
      writeFile(join(output,"runtime.log"),runtime.join("\n")),writeFile(join(output,"fixture-source.mjs"),source),writeFile(join(output,"source-resolution.json"),JSON.stringify(bundle.metafile.inputs,null,2))]);
  }
});
