import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { build } from 'esbuild';
import { fetch } from "./support/miniflare-fetch.mjs";
const require = createRequire(import.meta.url);
const { Miniflare, convertV4MiniflareOptions, Log, LogLevel } = createRequire(require.resolve('wrangler/package.json'))('miniflare');
const root = new URL('../', import.meta.url).pathname;
const seed = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ';
const source = `
import {routeCredentialRequest} from './src/credentials.ts';
import {routeServicesRequest} from './src/services-http.ts';
import {authenticate, UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey} from './src/account-auth.ts';
import {Kv} from 'accounts/server';
export {UserAccount, Organization, ApiKeyRecord, NonceStorage};
export default {async fetch(request,env){
  const url = new URL(request.url);
  if(url.pathname === '/__enroll') {
    const input = await request.json(); await ensureAccount(env,input.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(input.user).fetch('https://user.internal/authorization')).json();
    const key = await createApiKey(env,{kind:'api_key',userId:input.user,...auth.grant,subjectId:'api_key:'+input.user,
      credentialId:'fixture',capabilities:input.capabilities},'Synthetic TOTP journey');
    const token = 's_' + crypto.randomUUID().replaceAll('-','') + 'A'.repeat(11);
    await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,
      {userId:input.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
    return Response.json({...key,cookie:'nanocodex_account='+token});
  }
  const credential = await routeCredentialRequest(request,env,url);
  if(credential) return credential;
  if(url.pathname.startsWith('/v1/services')) return routeServicesRequest(request,env.NANOCODEX,await authenticate(request,env,url));
  return new Response(null,{status:404});
}};
`;
async function bundle(contents, resolveDir, wasm = false) {
  const result = await build({ stdin: { contents, resolveDir }, bundle: true, write: false,
    format: 'esm', platform: 'node', conditions: ['workerd'], external: ['cloudflare:*', 'node:*'],
    alias: { 'node-rsa': root + '../nanocodex/tools/browser/unsupportedNodeRsa.mjs' },
    plugins: [{ name: 'transports', setup(b) {
      b.onResolve({filter:/^nanocodex\/wasm$/},()=>({path:'./nanocodex.wasm',external:true}));
      b.onResolve({filter:/^\.\/whatsapp-runtime$/},()=>({path:root+'../egress/test/whatsapp/runtime.fixture.ts'}));
    }}],
  });
  return [{type:'ESModule',path:'worker.js',contents:result.outputFiles[0].text}, ...(wasm ? [{type:'CompiledWasm',path:'nanocodex.wasm',contents:await readFile(process.env.NANOCODEX_TEST_WASM ?? root+'../nanocodex/pkg-web/nanocodex_bg.wasm')}] : [])];
}
test('account HTTP TOTP enrollment and standalone requests enforce authenticated ownership', {timeout:120_000}, async()=>{
  const logs=[],trace=[],privateValues=[seed]; let calls=0;
  class CapturedLog extends Log {logWithLevel(level,message){logs.push(String(message));}}
  const common={compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat'],bindings:{ENVIRONMENT:'test'}};
  const mf=new Miniflare(convertV4MiniflareOptions({log:new CapturedLog(LogLevel.DEBUG),handleStructuredLogs:entry=>logs.push(JSON.stringify(entry)),workers:[
    {...common,name:'account',modules:await bundle(source,root),serviceBindings:{NANOCODEX:'egress'},durableObjects:{
      NANOCODEX_USERS:{className:'UserAccount',useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:'Organization',useSQLite:true},
      NANOCODEX_API_KEYS:{className:'ApiKeyRecord',useSQLite:true},NANOCODEX_AUTH:{className:'NonceStorage',useSQLite:true}}},
    {...common,name:'egress',modules:await bundle("export {default} from './src/egress.ts'; export * from './src/egress.ts';",root+'../egress/',true),durableObjects:{
      USER_CREDENTIALS:{className:'UserCredentialBroker',useSQLite:true},AGENT_SUBJECTS:{className:'AgentSubjectDirectory',useSQLite:true}},
      outboundService:async request=>{
        calls++; assert.equal(new URL(request.url).origin,'https://recipient.example.com');
        const code=request.headers.get('x-code');
        const epoch=Date.now()/1000;
        const expected=[-1,0,1].map(offset=>{
          const counter=Buffer.alloc(8);counter.writeBigUInt64BE(BigInt(Math.floor(epoch/30)+offset));
          const mac=createHmac('sha1','12345678901234567890').update(counter).digest();
          return String((mac.readUInt32BE(mac.at(-1)&15)&0x7fffffff)%1e6).padStart(6,'0');
        });
        assert.ok(expected.includes(code));privateValues.push(code);
        return new Response(code,{status:202,headers:{'x-code':code}});
      }},
  ]}));
  try{
    const base=await mf.ready;
    async function call(path,{method='POST',body,token,cookie,origin,status=200}={}){
      const response=await fetch(new URL(path,base),{method,headers:{'content-type':'application/json',...(token?{authorization:'Bearer '+token}:{}),...(cookie?{cookie}:{}),...(origin?{origin:origin==='same'?base.origin:origin}:{})},...(body===undefined?{}:{body:JSON.stringify(body)})});
      const text=await response.text();assert.ok(!text||text.startsWith('{'),text);const result=text?JSON.parse(text):null;
      assert.equal(response.status,status,text);assert.equal(response.headers.get('x-code'),null);
      if(path!=='/__enroll')trace.push({path,method,expected_status:status,observed_status:response.status,result});
      return result;
    }
    const capabilities=['agents:read','agents:write','tools:use','data:read','data:write'];
    const alice=await call('/__enroll',{body:{user:'11111111-1111-4111-8111-111111111111',capabilities}});
    const bob=await call('/__enroll',{body:{user:'22222222-2222-4222-8222-222222222222',capabilities}});
    const readOnly=await call('/__enroll',{body:{user:'11111111-1111-4111-8111-111111111111',capabilities:['data:read']}});
    for(const identity of [alice,bob,readOnly])privateValues.push(identity.token,identity.cookie.split('=')[1]);
    const path='/v1/credentials/vault/totp';
    const payload={name:'Synthetic account',origin:'https://recipient.example.com',seed,issuer:'Example',account:'synthetic@example.com'};
    await call(path,{body:payload,status:401});
    await call(path,{body:payload,token:readOnly.token,status:401});
    await call(path,{body:payload,cookie:alice.cookie,origin:'https://attacker.example',status:403});
    const entry=await call(path,{body:payload,cookie:alice.cookie,origin:'same',status:201});
    assert.equal(entry.digits,6);assert.equal(entry.period,30);assert.equal(entry.algorithm,'SHA1');
    const uriEntry=await call(path,{body:{name:'URI account',origin:payload.origin,otpauth_uri:`otpauth://totp/Example:a?secret=${seed}`},token:alice.token,status:201});
    const list=await call('/v1/services/vault',{method:'GET',token:alice.token});
    assert.equal(list.vault.length,2);assert.ok(list.vault.every(entry=>entry.kind==='totp'&&!('seed' in entry)&&!('code' in entry)));
    assert.deepEqual(await call('/v1/services/vault/'+entry.id,{method:'GET',token:alice.token}),{entry});
    assert.deepEqual(await call('/v1/services/vault',{method:'GET',token:bob.token}),{vault:[]});
    await call('/v1/services/vault/'+entry.id,{method:'GET',token:bob.token,status:404});
    const envelope={vault_id:entry.id,url:payload.origin+'/verify',method:'POST',headers:{'x-code':'{{NANOCODEX_VAULT_TOTP}}'}};
    assert.deepEqual(await call('/v1/services/vault/request',{body:envelope,token:alice.token}),{status:202,ok:true});
    const before=calls;
    await call('/v1/services/vault/request',{body:envelope,token:bob.token,status:409});
    await call('/v1/services/vault/request',{body:envelope,token:readOnly.token,status:403});
    await call('/v1/services/vault/request',{body:{...envelope,url:'https://attacker.example/verify'},token:alice.token,status:403});
    for(const bad of [{...payload,algorithm:null},{...payload,period:121},{...payload,digits:'6'},{...payload,code:'123456'},{...payload,seed:'INVALID!'},
      {name:'invalid URI',origin:payload.origin,otpauth_uri:`otpauth://totp/Example:a?secret=${seed}&issuer=Other`}])await call(path,{body:bad,token:alice.token,status:400});
    assert.equal(calls,before);
    await call(path+'/'+entry.id,{method:'DELETE',token:bob.token,status:204});
    assert.deepEqual(await call('/v1/services/vault/'+entry.id,{method:'GET',token:alice.token}),{entry});
    for(const id of [entry.id,uriEntry.id])await call(path+'/'+id,{method:'DELETE',token:alice.token,status:204});
    await call('/v1/services/vault/request',{body:envelope,token:alice.token,status:409});
    assert.deepEqual(await call('/v1/services/vault',{method:'GET',token:alice.token}),{vault:[]});
    assert.equal(calls,before);
    for(const secret of privateValues){assert.equal(JSON.stringify(trace).includes(secret),false);assert.equal(logs.join('\n').includes(secret),false);}
  }finally{
    await mkdir(root+'../../output/vault-totp-account',{recursive:true});
    await writeFile(root+'../../output/vault-totp-account/vault-totp-account-journey.json',JSON.stringify(trace,null,2));
    await writeFile(root+'../../output/vault-totp-account/vault-totp-account-worker.log',logs.join('\n'));await mf.dispose();
  }
});
