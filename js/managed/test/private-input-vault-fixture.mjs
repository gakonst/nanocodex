// Production encrypted Vault in local workerd; no live credentials or services.
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {join} from 'node:path';
import {build} from 'esbuild';
import {Miniflare,Log,LogLevel} from 'miniflare';

export async function encryptedVaultFixture() {
 const root=fileURLToPath(new URL('../../../',import.meta.url));
 // Production egress routes, broker validation, AES encryption and DO storage
 // remain intact. Only unrelated model/WhatsApp transports are disabled.
 const bundle=await build({
  stdin:{resolveDir:root,contents:`
   import Egress from './js/egress/src/egress.ts';
   import {UserCredentialBroker} from './js/egress/src/broker.ts';
   export {AgentSubjectDirectory} from './js/egress/src/egress.ts';
   export default class FixtureEgress extends Egress {
    async fetch(request) {
     if(new URL(request.url).pathname==='/__encrypted')
      return Response.json(await this.env.USER_CREDENTIALS.getByName('fixture-private-input-owner').fixtureEncryptedRows());
     const url=request.headers.get('x-fixture-url');
     if(!url)return new Response(null,{status:400});
     return super.fetch(new Request(url,request));
    }
   }
   export class EvidenceBroker extends UserCredentialBroker {
    constructor(ctx,env){super(ctx,env);this.fixtureStorage=ctx.storage;}
    async fixtureEncryptedRows(){
     const rows=await this.fixtureStorage.list({prefix:'vault-entry:'});
     return [...rows].map(([key,row])=>({key,encrypted:!!row.envelope?.ciphertext&&!!row.envelope?.iv,keys:Object.keys(row)}));
    }
   }`},
  bundle:true,write:false,format:'esm',platform:'node',conditions:['workerd'],target:'es2022',
  banner:{js:'import {createRequire} from "node:module";const require=createRequire("/worker.mjs");'},
  external:['cloudflare:*','node:*'],logLevel:'silent',
  alias:{'node-rsa':join(root,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},
  plugins:[{name:'unrelated-transports',setup(b){
   b.onResolve({filter:/^nanocodex\/(?:worker|wasm)$/},a=>({path:a.path,namespace:'unrelated'}));
   b.onLoad({filter:/.*/,namespace:'unrelated'},a=>({contents:a.path.endsWith('/wasm')?'export default null;':
    `export const ClaudeSubscription={open(){throw Error('Unrelated Claude transport disabled in Vault fixture')}};`,loader:'js'}));
   b.onResolve({filter:/^\.\/whatsapp-runtime$/},()=>({path:join(root,'js/egress/test/whatsapp/runtime.fixture.ts')}));
  }}],
 });
 const logs=[];
 class CapturedLog extends Log {logWithLevel(_level,message){logs.push(String(message));}}
 const mf=new Miniflare({log:new CapturedLog(LogLevel.DEBUG),handleStructuredLogs:entry=>{logs.push(JSON.stringify(entry));},workers:[{
  name:'egress',compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat'],
  modules:[{type:'ESModule',path:'worker.mjs',contents:bundle.outputFiles[0].text}],
  durableObjects:{USER_CREDENTIALS:{className:'EvidenceBroker',useSQLite:true},AGENT_SUBJECTS:{className:'AgentSubjectDirectory',useSQLite:true}},
  bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},
  outboundService:()=>{throw Error('Fixture must never contact real services');},
 }]});
 const base=await mf.ready,owner='fixture-private-input-owner',subject='P'.repeat(43);
 // Real HTTP avoids Miniflare's getWorker proxy, which hangs on some Node versions.
 // This local adapter is service-binding transport, not public account admission.
 // Preserve the production service-binding URL, identity path, body and headers.
 // Only transport is redirected to this fixture's isolated loopback listener.
 async function forwardRequest(request) {
  const url=new URL(request.url);
  assert.ok(['broker.internal','browser-vault.internal'].includes(url.hostname),'Unexpected Vault fixture destination');
  const headers=new Headers(request.headers);headers.set('x-fixture-url',request.url);
  return fetch(base,{method:request.method,headers,
   ...(['GET','HEAD'].includes(request.method)?{}:{body:await request.arrayBuffer()}),signal:AbortSignal.timeout(15000)});
 }
 async function call(url,body,method='POST',who=subject,headers={}) {
  const response=await forwardRequest(new Request(url,{method,headers:{'content-type':'application/json','x-nanocodex-subject':who,...headers},
   ...(body===undefined?{}:{body:JSON.stringify(body)})}));
  const result=await response.json();assert.ok(response.ok,`Vault HTTP ${response.status}: ${JSON.stringify(result)}`);return result;
 }
 try {await call(`https://broker.internal/subjects/${subject}`,{user_id:owner},'PUT');}
 catch(error){await mf.dispose();throw error;}
 return {mf,logs,owner,subject,call,forwardRequest,
  save:entry=>call('https://browser-vault.internal/v1/save',entry),
  materialize:input=>call('https://browser-vault.internal/v1/fields',input),
  list:()=>call(`https://broker.internal/users/${owner}/credentials/vault`,undefined,'GET'),
  create:(kind,payload,operationId)=>call(`https://broker.internal/users/${owner}/credentials/vault/${kind}`,payload,'POST',subject,operationId?{'x-nanocodex-operation-id':operationId}:{}),
  encryptedRows:async()=>{const r=await fetch(new URL('/__encrypted',base));assert.ok(r.ok);return r.json();},
  close:()=>mf.dispose(),
 };
}
