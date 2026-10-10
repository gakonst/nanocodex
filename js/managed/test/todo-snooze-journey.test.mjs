import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
// See todo-mail-journey.test.mjs: workerd closes after an early rejection that left the body unread.
const noReuse = method => method === "GET" ? {} : { connection: "close" };

// Real HTTP/account proxy/auth/TODO router/UserAccount SQLite DO. Only broker
// connector inventory is synthetic; any external provider call fails the test.
const source = `
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, authenticate } from "./src/account-auth.ts";
import { routeTodoRequest } from "./src/todo-inbox.ts";
import { routeManaged } from "../account/worker/managedProxy.ts";
import { Kv } from "accounts/server";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export default { async fetch(request, env) {
  const url = new URL(request.url);
  if (env.EDGE) return await routeManaged(request, env, url) ?? new Response("not_found", {status:404});
  if (url.pathname === "/__fixture") {
    const input = await request.json(); await ensureAccount(env,input.user,true);
    const stub = env.NANOCODEX_USERS.getByName(input.user);
    if (input.decision) return Response.json(await stub.proposeTodoDecision(input.decision));
    if (input.agent) { await stub.fetch("https://user.internal/agents",{method:"POST",body:JSON.stringify({agentId:input.agent})}); return Response.json({id:input.agent}); }
    if (input.session) {
      const token="s_"+"S".repeat(43);
      await Kv.durableObject(env.NANOCODEX_AUTH,{name:"account"}).set("session:"+token,{userId:input.user,authentication:"sms_otp",issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
      return Response.json({cookie:"nanocodex_account="+token});
    }
    const auth=await (await stub.fetch("https://user.internal/authorization")).json();
    return Response.json(await createApiKey(env,{kind:"api_key",userId:input.user,...auth.grant,subjectId:"api_key:"+input.user,credentialId:"fixture",capabilities:input.read_only?["agents:read"]:auth.grant.capabilities},"synthetic-snooze-journey"));
  }
  return await routeTodoRequest(request,env,url,await authenticate(request,env,url)) ?? new Response("not_found",{status:404});
} };
`;
const c1="A".repeat(43),c2="B".repeat(43),foreign="C".repeat(43);
test("snooze HTTP: two clients, durable undo/replay, auth, bounds and source ownership",{timeout:90_000},async()=>{
 const trace=[],inventoryTrace=[];let inventoryFailure=false,disconnected=false;
 const owner=crypto.randomUUID(),stranger=crypto.randomUUID();
 const broker=async request=>{
  const url=new URL(request.url);assert.equal(url.hostname,"broker.internal","no external provider access");assert.ok(url.pathname.endsWith("/connectors"));assert.equal(request.method,"GET");
  const user=decodeURIComponent(url.pathname.split("/")[2]);inventoryTrace.push({method:request.method,path:url.pathname});
  if(inventoryFailure)return new Response("Synthetic inventory failure",{status:503});
  const ids=disconnected?[]:user===owner?[c1,c2]:[foreign];
  const connections=ids.map(id=>({id,label:"Synthetic",capabilities:["gmail","gcalendar"]}));
  return Response.json({connectors:{gmail:{connected:true,connections},gcalendar:{connected:true,connections}}});
 };
 const bundled=await build({stdin:{contents:source,resolveDir:fileURLToPath(new URL("..",import.meta.url))},bundle:true,write:false,format:"esm",target:"es2022",platform:"browser",external:["cloudflare:workers","node:*"],alias:{"node-rsa":"./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"}});
 const script=bundled.outputFiles[0].text,persistence=fileURLToPath(new URL("../../../output/todo-snooze-store-"+crypto.randomUUID(),import.meta.url));
 const options={durableObjectsPersist:persistence,workers:[
  {name:"edge",script,modules:true,compatibilityDate:"2026-07-29",compatibilityFlags:["nodejs_compat"],bindings:{EDGE:true},serviceBindings:{NANOCODEX_BACKEND:"managed"}},
  {name:"managed",script,modules:true,compatibilityDate:"2026-07-29",compatibilityFlags:["nodejs_compat","enable_request_signal"],serviceBindings:{NANOCODEX:broker},durableObjects:{NANOCODEX_AUTH:{className:"NonceStorage",useSQLite:true},NANOCODEX_USERS:{className:"UserAccount",useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:"Organization",useSQLite:true},NANOCODEX_API_KEYS:{className:"ApiKeyRecord",useSQLite:true}}},
 ]};let mf=new Miniflare(options);
 try{
  let backend=await mf.getWorker("managed"),base=await mf.ready;
  async function fixture(input){const r=await backend.fetch("https://fixture.test/__fixture",{method:"POST",body:JSON.stringify(input)});assert.equal(r.status,200,await r.clone().text());return r.json();}
  const token=(await fixture({user:owner})).token,deviceB=(await fixture({user:owner})).token,other=(await fixture({user:stranger})).token,readOnly=(await fixture({user:owner,read_only:true})).token;
  async function request(path="",method="GET",body,credential=token,headers={}){
   const r=await fetch(new URL("/v1/todo"+path,base),{method,headers:{...(credential?{authorization:"Bearer "+credential}:{}),"content-type":"application/json",...noReuse(method),...headers},...(body===undefined?{}:{body:JSON.stringify(body)})});
   const text=await r.text();let data;try{data=JSON.parse(text);}catch{data=text;}
   trace.push({path,method,status:r.status,data});if(r.status===200)assert.equal(r.headers.get("cache-control"),"no-store");return {status:r.status,data};
  }
  async function call(path="",method="GET",body,credential=token,expected=200,headers={}){const r=await request(path,method,body,credential,headers);assert.equal(r.status,expected,`${method} ${path}: ${JSON.stringify(r.data)}`);return r.data;}
  const empty=await call();assert.deepEqual(empty.dispositions,[]);assert.equal(empty.disposition_coverage.complete,true);assert.equal(empty.disposition_coverage.limit,1000);
  const capture=(await call("","POST",{body:"Review synthetic report",operation_id:crypto.randomUUID()},token,201)).item;
  const row="capture:"+capture.id,until=Date.now()+3600_000,snooze={row_key:row,until,version:0,operation_id:crypto.randomUUID()};
  await call("/snooze","POST",snooze,null,401);await call("/snooze","POST",snooze,"ncx_live_invalid",401);await call("/snooze","POST",snooze,readOnly,403);await call("/snooze","POST",snooze,other,404);
  const {cookie}=await fixture({user:owner,session:true});
  for(const origin of [undefined,"https://unrelated.test"])await call("/snooze","POST",snooze,null,403,{cookie,...(origin?{origin}:{})});
  const connect=await backend.fetch("https://nanocodex.internal/v1/todo/snooze",{method:"POST",headers:{"x-nanocodex-connect-user":owner,"x-nanocodex-connect-grant-id":"0x"+"d".repeat(64),"x-nanocodex-connect-capabilities":JSON.stringify(["agents:write"]),"x-nanocodex-connect-connectors":"[]","x-nanocodex-connect-mcp-ids":"[]","content-type":"application/json"},body:JSON.stringify(snooze)});
  assert.equal(connect.status,403);trace.push({principal:"connect_grant",status:403,data:await connect.json()});
  const receipt=await call("/snooze","POST",snooze,null,200,{cookie,origin:base.origin});assert.deepEqual(receipt,{disposition:{row_key:row,until,version:1}});
  assert.deepEqual((await call("","GET",undefined,deviceB)).dispositions,[receipt.disposition]);assert.equal((await call("","GET",undefined,other)).dispositions.length,0);
  assert.deepEqual(await call("/snooze","POST",{...snooze,operation_id:snooze.operation_id.toUpperCase()},deviceB),receipt);
  assert.equal((await call("/snooze","POST",{...snooze,until:until+1},deviceB,409)).error,"operation_conflict");
  assert.equal((await call("/snooze","POST",{...snooze,operation_id:crypto.randomUUID()},deviceB,409)).error,"stale_disposition");
  const back=await call("/snooze","POST",{row_key:row,until:null,version:1,operation_id:crypto.randomUUID()},deviceB);assert.deepEqual(back,{disposition:{row_key:row,until:null,version:2}});
  assert.deepEqual(await call("/snooze","POST",snooze),receipt,"delayed replay returns original receipt");assert.deepEqual((await call()).dispositions,[back.disposition]);assert.equal((await call()).items[0].status,"captured");
  const decision=await fixture({user:owner,decision:{source_key:"synthetic-review",title:"Review",context:"Local request",source_label:"Synthetic",source_url:"",choices:[{id:"yes",title:"Approve"}]}});
  await call("/snooze","POST",{row_key:"decision:"+decision.id,until,version:0,operation_id:crypto.randomUUID()});assert.equal((await call()).decisions[0].status,"needs_you","snooze does not approve");
  const agent=crypto.randomUUID();await fixture({user:owner,agent});await call("/snooze","POST",{row_key:"agent:"+agent,until,version:0,operation_id:crypto.randomUUID()});
  await call("/snooze","POST",{row_key:"agent:"+crypto.randomUUID(),until,version:0,operation_id:crypto.randomUUID()},token,404);
  const mail={row_key:`mail:${c1}:t1`,until,version:0,operation_id:crypto.randomUUID()};await call("/snooze","POST",mail);await call("/snooze","POST",{...mail,row_key:`mail:${c2}:t1`,operation_id:crypto.randomUUID()});
  const event={row_key:`event:${c1}:${encodeURIComponent("team:calendar@example.test")}:e1`,until,version:0,operation_id:crypto.randomUUID()};await call("/snooze","POST",event);await call("/snooze","POST",{...event,row_key:`event:${c1}:primary:e1`,operation_id:crypto.randomUUID()});
  await call("/snooze","POST",{...mail,row_key:`mail:${foreign}:t1`,operation_id:crypto.randomUUID()},token,404);await call("/snooze","POST",mail,other,404);
  inventoryFailure=true;assert.deepEqual(await call("/snooze","POST",mail),{disposition:{row_key:mail.row_key,until,version:1}});
  await call("/snooze","POST",{...mail,until:until+1000,version:1,operation_id:crypto.randomUUID()},token,503);inventoryFailure=false;disconnected=true;
  await call("/snooze","POST",{...mail,until:until+1000,version:1,operation_id:crypto.randomUUID()},token,404);assert.deepEqual(await call("/snooze","POST",mail),{disposition:{row_key:mail.row_key,until,version:1}});
  assert.deepEqual(await call("/snooze","POST",{...mail,until:null,version:1,operation_id:crypto.randomUUID()}),{disposition:{row_key:mail.row_key,until:null,version:2}},"clear known presentation state works after disconnect");disconnected=false;
  for(const invalid of [
   {...snooze,row_key:"event:e1"},{...snooze,row_key:`event:${c1}:team%3acalendar:e1`},{...snooze,row_key:`mail:${c1}:../t1`},{...snooze,row_key:`mail:${c1}:`+"x".repeat(513)},{...snooze,row_key:`mail:${c1}:t1:extra`},{...snooze,row_key:"unknown:"+capture.id},
   {...snooze,until:Math.floor(Date.now()/1000)},{...snooze,until:Date.now()-1},{...snooze,until:Date.now()+367*86400_000},{...snooze,until:until+0.5},{...snooze,version:-1},{...snooze,version:0.5},{...snooze,external_action:"send"},{...snooze,until:"tomorrow"},{...snooze,until:undefined}
  ])await call("/snooze","POST",{...invalid,operation_id:crypto.randomUUID()},token,400);
  await call("/snooze","POST",{...snooze,operation_id:"not-a-uuid"},token,400);await call("/snooze?x=1","POST",snooze,token,404);await call("/snooze","GET",undefined,token,404);await call("/snooze","POST",{...snooze,row_key:"capture:"+crypto.randomUUID(),operation_id:crypto.randomUUID()},token,404);
  for(const body of ["{",JSON.stringify({row_key:"x".repeat(9000)})]){const r=await fetch(new URL("/v1/todo/snooze",base),{method:"POST",headers:{authorization:"Bearer "+token,...noReuse("POST")},body});assert.equal(r.status,400);trace.push({case:"invalid_or_oversize_json",status:r.status,data:await r.json()});}
  const race=await Promise.all([request("/snooze","POST",{row_key:row,until,version:2,operation_id:crypto.randomUUID()}),request("/snooze","POST",{row_key:row,until:until+1,version:2,operation_id:crypto.randomUUID()},deviceB)]);
  assert.deepEqual(race.map(r=>r.status).sort(),[200,409]);assert.equal(race.find(r=>r.status===409).data.error,"stale_disposition");
  const expiring={row_key:`mail:${c1}:expiring`,until:Date.now()+1500,version:0,operation_id:crypto.randomUUID()};
  const expiredReceipt=await call("/snooze","POST",expiring);
  const before=(await call()).dispositions;await mf.dispose();mf=new Miniflare(options);base=await mf.ready;backend=await mf.getWorker("managed");
  assert.deepEqual((await call("","GET",undefined,deviceB)).dispositions,before);assert.deepEqual(await call("/snooze","POST",snooze),receipt);
  // Reach hard capacity via public transport; no version-bearing tombstones lost.
  for(let i=before.length;i<1000;i++)await call("/snooze","POST",{row_key:`mail:${c1}:capacity${i}`,until,version:0,operation_id:crypto.randomUUID()});
  const full=await call();assert.equal(full.dispositions.length,1000);assert.equal(full.disposition_coverage.total,1000);assert.equal(full.disposition_coverage.complete,true);
  assert.equal((await call("/snooze","POST",{row_key:`mail:${c1}:overflow`,until,version:0,operation_id:crypto.randomUUID()},token,409)).error,"disposition_capacity");
  // The capacity journey has crossed expiry on the actual runtime clock. Keep
  // its version and return the same receipt even though its deadline is past.
  if(Date.now()<=expiring.until)await new Promise(resolve=>setTimeout(resolve,expiring.until-Date.now()+1));
  assert.ok(Date.now()>expiring.until);assert.deepEqual(await call("/snooze","POST",expiring),expiredReceipt);
  assert.deepEqual(full.dispositions.find(d=>d.row_key===expiring.row_key),expiredReceipt.disposition);
  const current=full.dispositions.find(d=>d.row_key===row);await call("/snooze","POST",{row_key:row,until:null,version:current.version,operation_id:crypto.randomUUID()});assert.equal((await call()).dispositions.find(d=>d.row_key===row).until,null);
 }finally{
  await mkdir(new URL("../../../output/",import.meta.url),{recursive:true});await writeFile(new URL("../../../output/todo-snooze-http-journey.json",import.meta.url),JSON.stringify({command:"node --test test/todo-snooze-journey.test.mjs",expected:"HTTP auth/ownership, CAS, durable replay/undo/restart, bounded complete snapshot, no provider action",trace,inventory_trace:inventoryTrace},null,2));await mf.dispose();await rm(persistence,{recursive:true,force:true});
 }
});
