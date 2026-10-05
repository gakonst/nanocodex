import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
const source = `export { UserAccount, Organization, ApiKeyRecord, NonceStorage } from "./src/account-auth.ts";
import { routeAccountRequest } from "./src/account-auth.ts";
import { routeConnectorRequest } from "./src/connectors.ts";
export default {async fetch(r,e){return await routeAccountRequest(r,e,new URL(r.url))??await routeConnectorRequest(r,e,new URL(r.url))??new Response(null,{status:404})}};`;
const b64=v=>Buffer.from(v).toString("base64url");
const hash=async v=>b64(await crypto.subtle.digest("SHA-256",new TextEncoder().encode(v)));
test("Google HTTP journey: sessions, explicit linking, isolation, invalid JWT and replay",{timeout:120000},async()=>{
 const trace=[], codes=new Map(), providerRequests=[];
 const output=new URL("../../../output/google-sign-in/",import.meta.url);
 const pair=await crypto.subtle.generateKey({name:"RSASSA-PKCS1-v1_5",modulusLength:2048,publicExponent:new Uint8Array([1,0,1]),hash:"SHA-256"},true,["sign","verify"]);
 const jwk={...await crypto.subtle.exportKey("jwk",pair.publicKey),kid:"synthetic-key",alg:"RS256",use:"sig"};
 const bundle=await build({stdin:{contents:source,resolveDir:fileURLToPath(new URL("..",import.meta.url))},bundle:true,write:false,format:"esm",target:"es2022",platform:"browser",external:["cloudflare:workers","node:*"],alias:{"nanocodex-tools/hosted":"../nanocodex-tools/src/hosted/index.ts","node-rsa":"./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"}});
 const provider=await build({stdin:{contents:'export { GoogleSignInProvider } from "./src/google-sign-in-provider.ts"; export default {fetch(){return new Response(null,{status:404})}};',resolveDir:fileURLToPath(new URL("../../egress",import.meta.url))},bundle:true,write:false,format:"esm",target:"es2022",platform:"browser",external:["cloudflare:workers","node:*"]});
 const outbound=async request=>{
  providerRequests.push(request.url);
  if(request.url==="https://www.googleapis.com/oauth2/v3/certs")return Response.json({keys:[jwk]});
  if(request.url!=="https://oauth2.googleapis.com/token")return new Response(null,{status:502});
  const body=await request.formData();
  if(body.get("code")==="provider-error")return Response.json({error:"synthetic-secret",access_token:"must-not-escape"},{status:400});
  if(body.get("code")==="provider-redirect")return new Response(null,{status:302,headers:{location:"https://untrusted.invalid/token"}});
  const pending=codes.get(body.get("code")); assert.ok(pending);codes.delete(body.get("code"));
  assert.equal(await hash(body.get("code_verifier")),pending.challenge);assert.equal(body.get("client_id"),"synthetic-client");assert.equal(body.get("client_secret"),"synthetic-secret");assert.equal(body.get("redirect_uri"),pending.redirect);
  const now=Math.floor(Date.now()/1000), claims={iss:"https://accounts.google.com",aud:"synthetic-client",sub:pending.sub,nonce:pending.nonce,iat:now,exp:now+300,email:"same@example.invalid",...pending.claims};
  const payload=`${b64(JSON.stringify({alg:"RS256",kid:jwk.kid,...pending.header}))}.${b64(JSON.stringify(claims))}`,signature=new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5",pair.privateKey,new TextEncoder().encode(payload)));if(pending.badSignature)signature[0]^=255;
  return Response.json({id_token:`${payload}.${b64(signature)}`,access_token:"unused-synthetic-token"});
 };
 const common={modules:true,compatibilityDate:"2026-07-29",compatibilityFlags:["nodejs_compat"]};
 const mf=new Miniflare({workers:[
  {name:"auth",...common,script:bundle.outputFiles[0].text,
   durableObjects:{NANOCODEX_AUTH:{className:"NonceStorage",useSQLite:true},NANOCODEX_USERS:{className:"UserAccount",useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:"Organization",useSQLite:true},NANOCODEX_API_KEYS:{className:"ApiKeyRecord",useSQLite:true}},
   serviceBindings:{GOOGLE_SIGN_IN:{name:"broker",entrypoint:"GoogleSignInProvider"},UNCONFIGURED_GOOGLE_SIGN_IN:{name:"empty-broker",entrypoint:"GoogleSignInProvider"},NANOCODEX:async()=>Response.json({address:"0x"+"1".repeat(40),created_at:1})},
   outboundService:request=>{assert.notEqual(request.url,"https://oauth2.googleapis.com/token","managed auth must not exchange the OAuth client secret");return outbound(request)}},
  {name:"broker",...common,script:provider.outputFiles[0].text,bindings:{ENVIRONMENT:"test",GOOGLE_OAUTH_CLIENT_ID:"synthetic-client",GOOGLE_OAUTH_CLIENT_SECRET:"synthetic-secret"},outboundService:outbound},
  {name:"empty-broker",...common,script:provider.outputFiles[0].text,bindings:{ENVIRONMENT:"test"},outboundService:()=>{throw new Error("unconfigured broker called Google")}},
  ...["no-binding","no-configuration"].map(name=>({name,...common,script:bundle.outputFiles[0].text,unsafeDirectSockets:[{host:"127.0.0.1",port:0}],
   durableObjects:{NANOCODEX_AUTH:{className:"NonceStorage",useSQLite:true},NANOCODEX_USERS:{className:"UserAccount",useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:"Organization",useSQLite:true},NANOCODEX_API_KEYS:{className:"ApiKeyRecord",useSQLite:true}},
   ...(name==="no-configuration"?{serviceBindings:{GOOGLE_SIGN_IN:{name:"empty-broker",entrypoint:"GoogleSignInProvider"}}}:{}),
   outboundService:()=>{throw new Error("unconfigured auth called Google")}})),
 ]});
 let base,ip=1;
 async function http(path,{method="GET",body,cookie,origin="same",expected=200}={}){
  const r=await fetch(new URL(path,base),{method,redirect:"manual",headers:{...(method==="POST"?{origin:origin==="same"?base.origin:origin,"content-type":"application/json","cf-connecting-ip":`192.0.2.${ip++}`} :{}),...(cookie?{cookie}:{})},...(body===undefined?{}:{body:JSON.stringify(body)})});
  const text=await r.text();trace.push({path:new URL(path,base).pathname,method,expected,observed:r.status});assert.equal(r.status,expected,text);assert.equal(r.headers.get("cache-control"),"no-store");let value;try{value=JSON.parse(text)}catch{value=null}return{value,headers:r.headers,cookie:r.headers.get("set-cookie")?.split(";")[0]};
 }
 const post=(action,body,options={})=>http(`/v1/auth/google/${action}`,{method:"POST",body,...options});
 async function begin(mode="browser",cookie,intent){
  const verifier=b64(crypto.getRandomValues(new Uint8Array(32))),start=await post("start",{mode,code_challenge:await hash(verifier),...(intent?{intent}:{})},{cookie});assert.equal(start.value.expires_in,300);
  const auth=await http(start.value.authorization_url,{cookie:start.cookie,expected:302}),google=new URL(auth.headers.get("location"));assert.equal(google.origin,"https://accounts.google.com");assert.equal(google.searchParams.get("scope"),"openid email");assert.equal(google.searchParams.get("code_challenge_method"),"S256");assert.match(google.searchParams.get("state"),/^signin\.[A-Za-z0-9_-]{43}$/);assert.equal(new URL(google.searchParams.get("redirect_uri")).pathname,"/v1/connectors/google/callback");return{id:start.value.attempt_id,proof:{attempt_id:start.value.attempt_id,code_verifier:verifier},browserCookie:auth.cookie,google};
 }
 function issue(f,sub,extras={}){const code=crypto.randomUUID();codes.set(code,{sub,nonce:f.google.searchParams.get("nonce"),challenge:f.google.searchParams.get("code_challenge"),redirect:f.google.searchParams.get("redirect_uri"),...extras});return`/v1/connectors/google/callback?state=${f.google.searchParams.get("state")}&code=${code}`}
 async function ready(f,sub,mode="browser",extras={}){const r=await http(issue(f,sub,extras),{cookie:f.browserCookie,expected:mode==="native"?302:200});if(mode==="native"){const u=new URL(r.headers.get("location"));assert.equal(u.protocol,"nanocodex:");assert.equal(u.hostname,"auth");assert.equal(u.pathname,"/google");assert.equal(u.searchParams.get("attempt_id"),f.id);assert.deepEqual([...u.searchParams.keys()].sort(),["attempt_id","completion_code","status"]);f.proof.completion_code=u.searchParams.get("completion_code");assert.match(f.proof.completion_code,/^[A-Za-z0-9_-]{43}$/)}return r}
 async function login(sub,{mode="browser",cookie,intent}={}){const f=await begin(mode,cookie,intent);await ready(f,sub,mode);assert.deepEqual((await post("status",f.proof)).value,{status:"ready"});if(mode==="native"){const stolen={...f.proof};delete stolen.completion_code;await post("complete",stolen,{cookie,expected:400});await post("complete",{...f.proof,completion_code:"x".repeat(43)},{cookie,expected:400})}const r=await post("complete",f.proof,{cookie});assert.equal(r.value.user.persistent,true);assert.match(r.cookie,/^nanocodex_account=s_/);assert.match(r.headers.get("set-cookie"),/HttpOnly/);return{...r,flow:f}}
 try{
  base=await mf.ready;
  const bindings=await mf.getBindings("auth");
  const client=await bindings.GOOGLE_SIGN_IN.fetch("https://google-sign-in.internal/v1/client");
  assert.equal(client.status,200);assert.deepEqual(await client.json(),{client_id:"synthetic-client"});
  const missing=await bindings.UNCONFIGURED_GOOGLE_SIGN_IN.fetch("https://google-sign-in.internal/v1/client");
  assert.equal(missing.status,503);assert.deepEqual(await missing.json(),{error:"google_sign_in_unavailable"});
  for(const name of ["no-binding","no-configuration"]){
   const origin=(await mf.unsafeGetDirectURL(name)).origin;
   const response=await fetch(origin+"/v1/auth/google/start",{method:"POST",headers:{origin,"content-type":"application/json"},body:JSON.stringify({mode:"browser",code_challenge:"a".repeat(43)})});
   assert.equal(response.status,503);assert.deepEqual(await response.json(),{error:"google_sign_in_unavailable"});
   trace.push({path:"/v1/auth/google/start",scenario:name,expected:503,observed:response.status});
  }
  const exchange={client_id:"synthetic-client",code:"provider-error",code_verifier:"a".repeat(43),redirect_uri:"https://account.example/v1/connectors/google/callback"};
  for(const [input,expected] of [[{...exchange,client_id:"rotated-client"},400],[{...exchange,redirect_uri:"https://account.example/v1/connectors/gmail/callback"},400],[{...exchange,token_url:"https://untrusted.invalid/token"},400],[exchange,502],[{...exchange,code:"provider-redirect"},502]]){
   const response=await bindings.GOOGLE_SIGN_IN.fetch("https://google-sign-in.internal/v1/token",{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(input)});
   assert.equal(response.status,expected);assert.deepEqual(await response.json(),{error:expected===400?"invalid_google_request":"google_authorization_failed"});
   trace.push({path:"/v1/token",boundary:"private-broker",expected,observed:response.status});
  }
  assert.ok(providerRequests.every(url=>url==="https://oauth2.googleapis.com/token"));
  const identityCode=crypto.randomUUID(), identityProof="b".repeat(43);
  codes.set(identityCode,{sub:"private-broker-proof",nonce:"private-nonce",challenge:await hash(identityProof),redirect:exchange.redirect_uri});
  const identity=await bindings.GOOGLE_SIGN_IN.fetch("https://google-sign-in.internal/v1/token",{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({...exchange,code:identityCode,code_verifier:identityProof})});
  assert.equal(identity.status,200);const identityOnly=await identity.json();assert.deepEqual(Object.keys(identityOnly),["id_token"]);assert.equal(typeof identityOnly.id_token,"string");
  await http("/v1/connectors/google/callback?state="+"a".repeat(43)+"&code=connector-code",{expected:401});
  await http("/v1/connectors/google/callback?state=signin.invalid&code=login-code",{expected:400});
  await post("start",{mode:"browser",code_challenge:"a".repeat(43)},{origin:"https://evil.invalid",expected:403});
  await post("start",{mode:"browser",code_challenge:"a".repeat(43),intent:"link"},{expected:401});
  const anon=await http("/v1/me"),alice=await login("alice",{cookie:anon.cookie});assert.notEqual(alice.value.user.id,anon.value.user.id,"fresh account prevents SMS promotion race");await post("complete",alice.flow.proof,{expected:400});
  const returning=await login("alice",{mode:"native"});assert.equal(returning.value.user.id,alice.value.user.id);
  const cross=await begin("native");await ready(cross,"cross-attempt","native");await post("complete",{...cross.proof,completion_code:returning.flow.proof.completion_code},{expected:400});await post("complete",cross.proof);await post("complete",cross.proof,{expected:400});
  const nativeCancelled=await begin("native");await ready(nativeCancelled,"cancelled-native","native");await post("cancel",nativeCancelled.proof,{expected:204});await post("cancel",nativeCancelled.proof,{expected:204});await post("complete",nativeCancelled.proof,{expected:400});assert.deepEqual((await post("status",nativeCancelled.proof)).value,{status:"cancelled"});
  const bob=await login("bob",{cookie:alice.cookie});assert.notEqual(bob.value.user.id,alice.value.user.id,"same email never links");assert.equal((await http("/v1/me",{cookie:bob.cookie})).value.user.id,bob.value.user.id);
  const key=await http("/v1/api-keys",{method:"POST",cookie:bob.cookie,body:{label:"Synthetic native key"},expected:201});assert.match(key.value.api_key,/^ncx_live_/);
  const linked=await login("bob-alternate",{cookie:bob.cookie,intent:"link"});assert.equal(linked.value.user.id,bob.value.user.id);
  const conflict=await begin("browser",linked.cookie,"link");await ready(conflict,"alice");await post("complete",conflict.proof,{cookie:linked.cookie,expected:409});
  const changed=await begin("browser",linked.cookie,"link");await ready(changed,"third");await post("complete",changed.proof,{cookie:returning.cookie,expected:403});
  const guarded=await begin();assert.deepEqual((await post("status",guarded.proof)).value,{status:"pending"});await post("complete",guarded.proof,{expected:409});await post("status",{...guarded.proof,code_verifier:"x".repeat(43)},{expected:400});const cb=issue(guarded,"guarded");await http(cb,{expected:400});await http(cb,{cookie:guarded.browserCookie});await http(cb,{cookie:guarded.browserCookie,expected:400});
  for(const invalid of [...[{iss:"https://evil.invalid"},{aud:"foreign"},{exp:1},{nonce:"wrong"},{azp:"foreign"}].map(claims=>({claims})),{badSignature:true},{header:{alg:"none"}}]){const f=await begin();await ready(f,"invalid","browser",invalid);assert.deepEqual((await post("status",f.proof)).value,{status:"failed",error:"google_authorization_failed"});await post("complete",f.proof,{expected:400})}
  const denied=await begin("native");await http(`/v1/connectors/google/callback?state=${denied.google.searchParams.get("state")}&error=access_denied`,{cookie:denied.browserCookie,expected:302});assert.deepEqual((await post("status",denied.proof)).value,{status:"failed",error:"google_access_denied"});
  const cancelled=await begin();await post("cancel",cancelled.proof,{expected:204});assert.deepEqual((await post("status",cancelled.proof)).value,{status:"cancelled"});await http(issue(cancelled,"cancelled"),{cookie:cancelled.browserCookie,expected:400});
 }finally{await mkdir(output,{recursive:true});await writeFile(new URL("http-trace.json",output),JSON.stringify(trace,null,2));await mf.dispose()}
});
