// Actual account managed proxy -> managed HTTP authorization, synthetic enrollment only.
import assert from 'node:assert/strict';
import {build} from 'esbuild';
import {Miniflare} from 'miniflare';
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {encryptedVaultFixture} from './private-input-vault-fixture.mjs';
import { fetch } from "./support/miniflare-fetch.mjs";
const root=fileURLToPath(new URL('..',import.meta.url)),output=join(root,'../../output/private-input-tui');
await mkdir(output,{recursive:true});const trace=[];
async function bundle(contents){const assets=[];let assetIndex=0;const b=await build({stdin:{contents,resolveDir:root},bundle:true,write:false,format:'esm',platform:'node',conditions:['workerd'],target:'es2022',external:['cloudflare:*','node:*'],banner:{js:'import {createRequire} from "node:module";const require=createRequire("/worker.mjs");'},alias:{'nanocodex-tools/user-data':join(root,'../nanocodex-tools/dist/user-data.js'),'node-rsa':join(root,'../nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'wasm',setup(b){b.onResolve({filter:/(?:\.wasm$|^nanocodex\/wasm$)/},async a=>{const path=a.path==='nanocodex/wasm'?join(root,'../nanocodex/pkg-web/nanocodex_bg.wasm'):join(a.resolveDir,a.path);const name=`fixture-${assetIndex++}.wasm`;assets.push({type:'CompiledWasm',path:name,contents:await readFile(path)});return {path:'./'+name,external:true};});}}],logLevel:'silent'});return [{type:'ESModule',path:'worker.mjs',contents:b.outputFiles[0].text},...assets];}
const managed=await bundle(`import worker,{DurableAgentSession,AccountHostedTools} from './src/index.ts';
import {UserAccount,Organization,ApiKeyRecord,NonceStorage,ensureAccount,createApiKey} from './src/account-auth.ts';
import {Kv} from 'accounts/server';export {DurableAgentSession,AccountHostedTools,UserAccount,Organization,ApiKeyRecord,NonceStorage};
export default {async fetch(request,env,ctx){if(new URL(request.url).pathname==='/__enroll'){const b=await request.json();await ensureAccount(env,b.user,true);const auth=await(await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();const key=await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,credentialId:'fixture',capabilities:b.capabilities},'Synthetic private-input test');const token='s_'+crypto.randomUUID().replaceAll('-','')+'A'.repeat(11);await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,{userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});return Response.json({...key,cookie:'nanocodex_account='+token});}return worker.fetch(request,env,ctx);}};`);
const account=await bundle(`import {routeManaged} from '../account/worker/managedProxy.ts';export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url))??new Response(null,{status:404});}};`);
const common={compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat','enable_request_signal']};
const vault=await encryptedVaultFixture(),forwarded=[];
const mf=new Miniflare({workers:[{...common,name:'account',modules:account,serviceBindings:{NANOCODEX_BACKEND:'managed'}},{...common,name:'managed',modules:managed,serviceBindings:{NANOCODEX:request=>{forwarded.push({url:request.url,method:request.method,operation:request.headers.get('x-nanocodex-operation-id')});return vault.forwardRequest(request);}},bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},durableObjects:{NANOCODEX_SESSIONS:{className:'DurableAgentSession',useSQLite:true},NANOCODEX_USERS:{className:'UserAccount',useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:'Organization',useSQLite:true},NANOCODEX_API_KEYS:{className:'ApiKeyRecord',useSQLite:true},NANOCODEX_AUTH:{className:'NonceStorage',useSQLite:true},NANOCODEX_ACCOUNT_TOOLS:{className:'AccountHostedTools',useSQLite:true}},r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES']}]});
try{
 const base=await mf.ready,backend=await mf.getWorker('managed');
 const owner='11111111-1111-4111-8111-111111111111',other='22222222-2222-4222-8222-222222222222';
 const scopes=['agents:read','agents:write','tools:use'];
 async function enroll(user,capabilities){const r=await backend.fetch('https://fixture.test/__enroll',{method:'POST',body:JSON.stringify({user,capabilities})});assert.equal(r.status,200);return r.json();}
 const alice=await enroll(owner,scopes),bob=await enroll(other,scopes),noTools=await enroll(owner,['agents:read','agents:write']);
 const create=await fetch(new URL('/v1/agents',base),{method:'POST',headers:{authorization:'Bearer '+alice.token,'content-type':'application/json'},body:JSON.stringify({settings:{model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false}})});assert.equal(create.status,201,await create.clone().text());const {agent_id:agent}=await create.json();
 async function call(label,resource,options={}){
  const {expected,token=alice.token,cookie,origin,query='',headers={}}=options;
  const path=`/v1/agents/${agent}/${resource}${query}`;
  const body=resource==='browser-vault/takeover'?{challenge_id:crypto.randomUUID(),action:'describe'}:resource==='browser-vault/challenge'?{challenge_id:crypto.randomUUID(),code:'123456'}:{request_id:crypto.randomUUID(),action:'describe'};
  const r=await fetch(new URL(path,base),{method:'POST',headers:{...(token?{authorization:'Bearer '+token}:{}),...(cookie?{cookie}:{}),...(origin?{origin:origin==='same'?base.origin:origin}:{}),'content-type':'application/json',...headers},body:JSON.stringify(body)});const data=await r.json();trace.push({label,path,status:r.status,expected});assert.equal(r.status,expected,`${label}: ${JSON.stringify(data)}`);
 }
 for(const resource of ['browser-vault/takeover','browser-vault/challenge','secure-input']){
  await call('anonymous rejected',resource,{token:null,expected:401});
  await call('invalid bearer rejected',resource,{token:'invalid',expected:401});
  await call('tools scope required',resource,{token:noTools.token,expected:403});
  await call('foreign owner rejected',resource,{token:bob.token,expected:404});
  await call('cookie without origin rejected',resource,{token:null,cookie:alice.cookie,expected:403});
  await call('cookie forged origin rejected',resource,{token:null,cookie:alice.cookie,origin:'https://forged.example.test',expected:403});
  await call('authenticated owner reaches private runtime; missing request closed',resource,{expected:409,headers:{'x-nanocodex-user-id':other,'x-nanocodex-subject':'Z'.repeat(43)}});
  await call('same-origin cookie reaches private runtime; missing request closed',resource,{token:null,cookie:alice.cookie,origin:'same',expected:409});
 }
 assert.equal(trace.length,24,'Retain every existing private admission assertion');
 const kinds=[
  {kind:'login',payload:{name:'Synthetic login',username:'fixture@example.test',password:'synthetic-http-password'}},
  {kind:'api_key',payload:{name:'Synthetic key',api_key:'synthetic-http-api-key'}},
  {kind:'card',payload:{name:'Synthetic card',card_number:'4111111111111111',expiry_month:'09',expiry_year:'2031',billing_zip:'10001'}},
  {kind:'address',payload:{name:'Synthetic address',address_line_1:'1 Fixture Way',address_line_2:'Suite Fixture',city:'Fixture City',state:'Fixture State',zip:'10558',country:'GR'}},
  {kind:'phone',payload:{name:'Synthetic phone',phone_number:'+306900000000'}},
 ];
 // Stable UUIDv5 mirrors the native intake contract: retry the same turn ID.
 function operationId(kind){const bytes=createHash('sha1').update(Buffer.from('6ba7b8119dad11d180b400c04fd430c8','hex')).update('synthetic-vault-intake:'+kind).digest().subarray(0,16);bytes[6]=(bytes[6]&15)|80;bytes[8]=(bytes[8]&63)|128;const h=bytes.toString('hex');return [h.slice(0,8),h.slice(8,12),h.slice(12,16),h.slice(16,20),h.slice(20)].join('-');}
 async function credential(label,kind,payload,{token=alice.token,operation=operationId(kind),expected=201}={}){
  const path='/v1/credentials/vault/'+kind;
  const r=await fetch(new URL(path,base),{method:'POST',headers:{...(token?{authorization:'Bearer '+token}:{}),'content-type':'application/json','x-nanocodex-operation-id':operation,'x-nanocodex-user-id':other},body:JSON.stringify(payload)});
  const data=await r.json();trace.push({label,path,status:r.status,expected,...(r.ok?{id:data.id,kind:data.kind}:{})});
  assert.equal(r.status,expected,label);return data;
 }
 for(const item of kinds){
  const saved=await credential(item.kind+' valid payload saved through managed route',item.kind,item.payload);
  assert.match(saved.id,/^[A-Za-z0-9_-]{22,64}$/);assert.equal(saved.kind,item.kind);item.id=saved.id;
  const forwardedCreate=forwarded.at(-1);assert.equal(forwardedCreate.url,'https://broker.internal/users/'+owner+'/credentials/vault/'+item.kind);assert.equal(forwardedCreate.operation,operationId(item.kind));
  const replay=await credential(item.kind+' stable UUIDv5 replay returns same ID',item.kind,item.payload);assert.equal(replay.id,saved.id);
  const conflict=await credential(item.kind+' changed payload rejected for same operation',item.kind,{...item.payload,name:'Changed synthetic name'},{expected:409});assert.equal(conflict.error,'vault_operation_conflict');
  for(const [label,token] of [['anonymous',null],['missing tools scope',noTools.token]]){
   const before=forwarded.length;await credential(item.kind+' '+label+' rejected',item.kind,item.payload,{token,expected:401});assert.equal(forwarded.length,before,'Unauthorized request never reaches broker');
  }
  const before=forwarded.length;await credential(item.kind+' unexpected field rejected before broker',item.kind,{...item.payload,unexpected:'synthetic'},{expected:400});assert.equal(forwarded.length,before,'Managed payload validation rejects before broker');
  const invalid=await credential(item.kind+' invalid operation header rejected',item.kind,item.payload,{operation:'invalid-operation',expected:400});assert.equal(invalid.error,'invalid_operation_id');
 }
 // Public credential metadata reads bind the authenticated user's real browser subject.
 for(const [label,principal] of [['owner',alice],['other owner',bob]]){
  const r=await fetch(new URL('/v1/credentials',base),{headers:{authorization:'Bearer '+principal.token}});trace.push({label:label+' credential subject binding',path:'/v1/credentials',status:r.status,expected:200});assert.equal(r.status,200);await r.arrayBuffer();
 }
 const subjectFor=user=>createHash('sha256').update('browser-model-v1:'+user).digest('base64url');
 for(const item of kinds){
  const fields=Object.keys(item.payload).filter(key=>key!=='name');
  for(const [who,expected] of [[owner,200],[other,403]]){
   const r=await vault.forwardRequest(new Request('https://browser-vault.internal/v1/fields',{method:'POST',headers:{'content-type':'application/json','x-nanocodex-subject':subjectFor(who)},body:JSON.stringify({vault_id:item.id,expected_origin:'https://fixture.example',fields})}));
   trace.push({label:item.kind+(who===owner?' authenticated owner materializes saved fields':' foreign owner cannot materialize'),path:'/v1/fields',status:r.status,expected,id:item.id,kind:item.kind});assert.equal(r.status,expected,'Materialization ownership');
   const data=await r.json();if(who===owner){for(const field of fields)assert.equal(data.values[field],item.payload[field],item.kind+' saved field round trip');assert.equal(Object.hasOwn(data.values,'cvv'),false);}
  }
 }
 console.log('PASS actual account proxy, managed admission and encrypted Vault: '+trace.length+' HTTP assertions (original 24 preserved)');
} finally {await writeFile(join(output,'admission-trace.json'),JSON.stringify({command:'node js/managed/test/private-input-admission-journey.mjs',boundaries:['production account routeManaged','production managed HTTP account resolver','production session ownership/private route','production routeCredentialRequest payload validation and operation header forwarding','production egress and encrypted Vault broker'],fixture:['synthetic account enrollment','empty retained private browser requests'],trace},null,2)+'\n');await mf.dispose();await vault.close();}
