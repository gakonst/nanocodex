import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

test("WhatsApp public HTTP authorization and bounded requests", {timeout:90000}, async () => {
  const bundle = await build({entryPoints:[fileURLToPath(new URL("fixtures/whatsapp-public-worker.ts", import.meta.url))],
    bundle:true, write:false, format:"esm", target:"es2022", platform:"browser", external:["cloudflare:workers","node:*"],
    alias:{"node-rsa":"./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"}});
  const trace = [];
  const mf = new Miniflare({workers:[{name:"edge", modules:true, compatibilityDate:"2026-07-29", serviceBindings:{MANAGED:"managed"}, script:`export default {fetch(r,e) { const u=new URL(r.url); if(u.pathname.startsWith("/__connect/")){u.protocol="https:"; u.host="nanocodex.internal"; u.port=""; u.pathname=u.pathname.slice(10); return e.MANAGED.fetch(new Request(u,r));} return e.MANAGED.fetch(r); }}`}, {name:"managed", script:bundle.outputFiles[0].text, modules:true,
    compatibilityDate:"2026-07-29", compatibilityFlags:["nodejs_compat"],
    durableObjects:Object.fromEntries([["NANOCODEX_AUTH","NonceStorage"],["NANOCODEX_USERS","UserAccount"],
      ["NANOCODEX_ORGANIZATIONS","Organization"],["NANOCODEX_API_KEYS","ApiKeyRecord"]].map(([key,className])=>[key,{className,useSQLite:true}])),
    serviceBindings:{NANOCODEX:"broker"}},
    {name:"broker", modules:true, compatibilityDate:"2026-07-29", script:`export default {async fetch(r) {
      return Response.json({url:r.url, headers:Object.fromEntries(r.headers), path:new URL(r.url).pathname, query:new URL(r.url).search, method:r.method,
      body:r.method === 'POST' ? await r.json() : null}, {headers:{'x-upstream-private':'hidden'}});
    }}`}]});
  try {
    const base = await mf.ready;
    const user = "11111111-1111-4111-8111-111111111111";
    const issue = async capabilities => {
      const r = await fetch(new URL("/__fixture",base), {method:"POST",body:JSON.stringify({user,capabilities})});
      assert.equal(r.status,200,await r.clone().text()); return r.json();
    };
    const owner = await issue();
    const scoped = await issue(["tools:use"]);
    const operation_id = "22222222-2222-4222-8222-222222222222";
    const connection = "c".repeat(43);
    const connectHeaders = {"x-nanocodex-connect-user":user,"x-nanocodex-connect-grant-id":"0x"+"a".repeat(64),
      "x-nanocodex-connect-capabilities":JSON.stringify(["tools:use"]),
      "x-nanocodex-connect-connectors":"[]","x-nanocodex-connect-mcp-ids":"[]"};
    const principal = await fetch(new URL("/__connect/__fixture/principal",base),{headers:connectHeaders});
    assert.deepEqual(await principal.json(),{kind:"connect_grant"});
    trace.push({name:"trusted ingress resolves genuine Connect grant",expected:"connect_grant",observed:"connect_grant"});
    async function call(name,path,expected,init={}) {
      const r=await fetch(new URL("/v1/connectors/whatsapp"+path,base), {...init,headers:{authorization:`Bearer ${owner.token}`,...init.headers}});
      const body=await r.json(); trace.push({name,expected,observed:r.status,body});
      assert.equal(r.status,expected,JSON.stringify(body)); return {r,body};
    }
    const status = await call("owner status","",200);
    assert.equal(status.r.headers.get("cache-control"),"no-store");
    assert.equal(status.r.headers.get("pragma"),"no-cache");
    assert.equal(status.r.headers.get("referrer-policy"),"no-referrer");
    assert.equal(status.r.headers.get("x-upstream-private"),null);
    assert.equal(status.body.path,`/users/${user}/connectors/whatsapp`);
    const start = await call("owner start","/start",200,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({operation_id,phone:"+15555550123"})});
    assert.deepEqual(start.body.body,{operation_id,phone:"+15555550123"});
    const pairing = await call("owner pairing",`/pairing?operation_id=${operation_id}`,200);
    assert.equal(pairing.body.query,`?operation_id=${operation_id}`);
    await call("owner disconnect",`/connections/${connection}`,200,{method:"DELETE"});
    await call("scoped key denied","",401,{headers:{authorization:`Bearer ${scoped.token}`}});
    await call("anonymous denied","",401,{headers:{authorization:""}});
    const session = {authorization:"",cookie:owner.cookie,origin:base.origin};
    await call("same-origin owner session","",200,{headers:session});
    for (const [path,method] of [["","GET"],["/start","POST"],[`/pairing?operation_id=${operation_id}`,"GET"],[`/connections/${connection}`,"DELETE"]]) {
      await call("cross-site session "+method+path,path,403,{method,headers:{...session,origin:"https://evil.example"}});
      await call("missing session origin "+method+path,path,403,{method,headers:{authorization:"",cookie:owner.cookie}});
      await call("scoped key "+method+path,path,401,{method,headers:{authorization:`Bearer ${scoped.token}`}});
      const delegated = await fetch(new URL("/__connect/v1/connectors/whatsapp"+path,base), {method,headers:connectHeaders});
      const result=await delegated.json(); trace.push({name:"Connect denied "+method+path,expected:401,observed:delegated.status,body:result});
      assert.equal(delegated.status,401,JSON.stringify(result));
    }
    const browserRead = {authorization:"",cookie:owner.cookie,"sec-fetch-site":"same-origin","x-nanocodex-request":"1"};
    for (const path of ["",`/pairing?operation_id=${operation_id}`]) {
      await call("browser GET without Origin "+path,path,200,{headers:browserRead});
      for (const [name,headers] of [
        ["cross-site metadata",{...browserRead,"sec-fetch-site":"cross-site"}],
        ["explicit header only",{authorization:"",cookie:owner.cookie,"x-nanocodex-request":"1"}],
        ["fetch metadata only",{authorization:"",cookie:owner.cookie,"sec-fetch-site":"same-origin"}],
      ]) await call(name+" "+path,path,403,{headers});
    }
    await call("POST metadata cannot replace Origin","/start",403,{method:"POST",headers:{...browserRead,"content-type":"application/json"},body:JSON.stringify({operation_id,phone:"+15555550123"})});
    await call("DELETE metadata cannot replace Origin",`/connections/${connection}`,403,{method:"DELETE",headers:browserRead});
    for (const path of ["?extra=1",`/pairing?operation_id=${operation_id}&extra=1`,`/pairing?operation_id=${operation_id}&operation_id=${operation_id}`,"/pairing","/pairing?operation_id=invalid","/start?extra=1"]) {
      await call("invalid query "+path,path,400,{method:path.startsWith("/start")?"POST":"GET"});
    }
    const valid = JSON.stringify({operation_id,phone:"+15555550123"});
    for (const [name,body,type] of [["unknown field",JSON.stringify({operation_id,phone:"+15555550123",extra:true}),"application/json"],
      ["bad phone",JSON.stringify({operation_id,phone:"555"}),"application/json"],
      ["bad id",JSON.stringify({operation_id:"bad",phone:"+15555550123"}),"application/json"],
      ["array","[]","application/json"],["malformed","{","application/json"],
      ["wrong media",valid,"text/plain"],["oversized",valid+" ".repeat(1025),"application/json"]]) {
      await call(name,"/start",400,{method:"POST",headers:{"content-type":type},body});
    }
    await call("body exactly 1024 bytes","/start",200,{method:"POST",headers:{"content-type":"application/json"},body:valid+" ".repeat(1024-Buffer.byteLength(valid))});
    await call("body 1025 bytes","/start",400,{method:"POST",headers:{"content-type":"application/json"},body:valid+" ".repeat(1025-Buffer.byteLength(valid))});
    for (const path of ["",`/pairing?operation_id=${operation_id}`]) {
      await call("contradictory foreign Origin "+path,path,403,{headers:{...browserRead,origin:"https://evil.example"}});
    }
    async function tool(name,input,expected=200,http=200) {
      const r=await fetch(new URL("/__fixture/tool",base),{method:"POST",body:JSON.stringify(input)});
      const body=await r.json(); trace.push({name,expected,http:r.status,observed:body.status,body});
      assert.equal(r.status,http,JSON.stringify(body)); if(http===200) assert.equal(body.status,expected,JSON.stringify(body)); return body;
    }
    for (const path of ["/chats?limit=20","/search?q="+encodeURIComponent("こんにちは café")]) {
      const result=await tool("tool read "+path,{request:{path,connection_id:connection}});
      assert.equal(result.data.url,"https://whatsapp.internal"+path);
      assert.equal(result.data.headers.authorization,"Bearer NANOCODEX_PROVIDER_CREDENTIAL");
      assert.equal(result.data.headers["x-nanocodex-subject"],"s".repeat(43));
      assert.equal(result.data.headers["x-nanocodex-connector-connection"],connection);
    }
    const implicit=await tool("single granted selector injected",{request:{path:"/chats"}});
    assert.equal(implicit.data.headers["x-nanocodex-connector-connection"],connection);
    await tool("tool unavailable",{available:false,request:{path:"/chats"}},403,403);
    await tool("egress grant denied",{grant:false,request:{path:"/chats"}},403);
    await tool("egress requires subject",{subject:false,request:{path:"/chats"}},403);
    await tool("selector outside grant",{request:{path:"/chats",connection_id:"d".repeat(43)}},403);
    for (const path of ["/pairing","/start","/logout","/send"]) {
      await tool("private tool GET "+path,{request:{path}},403);
      await tool("private tool POST "+path,{request:{path,method:"POST",body:{}}},403);
    }
    for (const path of ["//whatsapp.internal.evil/chats","https://whatsapp.internal.evil/chats"]) await tool("tool destination escape",{request:{path}},400,400);
    async function egress(name,url,headers={},expected=403) {
      const r=await fetch(new URL("/__fixture/egress",base),{method:"POST",body:JSON.stringify({url,headers})});
      const body=await r.json(); trace.push({name,expected,observed:r.status,body}); assert.equal(r.status,expected,JSON.stringify(body));return body;
    }
    await egress("other internal destination","https://other.internal/chats");
    await egress("Vault mode remains denied","https://whatsapp.internal/chats",{"x-nanocodex-vault-id":"v".repeat(22)});
    await egress("virtual HTTP denied","http://whatsapp.internal/chats");
    await egress("virtual fragment denied","https://whatsapp.internal/chats#private");
    await egress("virtual credentials denied","https://user:pass@whatsapp.internal/chats");
    const lookalike=await egress("lookalike uses public gateway","https://whatsapp.internal.evil/chats",{},200);
    assert.equal(lookalike.url,"https://public-egress.internal/v1/request");
    assert.equal(lookalike.headers.authorization,undefined);
    assert.equal(lookalike.headers["x-nanocodex-connector-connection"],undefined);
    assert.equal(lookalike.headers["x-nanocodex-target-url"],"https://whatsapp.internal.evil/chats");
    await call("unsupported method","",405,{method:"POST"});
    await call("invalid connection","/connections/short",404,{method:"DELETE"});

  } finally {
    await mf.dispose();
    const output=new URL("../../../output/whatsapp-public/",import.meta.url);
    await mkdir(output,{recursive:true}); await writeFile(new URL("trace.json",output),JSON.stringify(trace,null,2));
  }
});

test("agent starts native WhatsApp linking over HTTP with private code isolation", {timeout:90000}, async () => {
  const root = fileURLToPath(new URL("../../egress/", import.meta.url));
  const [managed, broker] = await Promise.all([
    build({entryPoints:[fileURLToPath(new URL("fixtures/whatsapp-public-worker.ts", import.meta.url))],
      bundle:true, write:false, format:"esm", target:"es2022", platform:"browser", external:["cloudflare:workers","node:*"],
      alias:{"node-rsa":"./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"}}),
    build({entryPoints:[root+"test/whatsapp/broker.worker.ts"], bundle:true, write:false, format:"esm", platform:"node",
      external:["cloudflare:*","node:*"], alias:{"node-rsa":root+"../nanocodex/tools/browser/unsupportedNodeRsa.mjs"},
      plugins:[{name:"upstream-only", setup(b) {
        b.onResolve({filter:/^nanocodex\/wasm$/},()=>({path:"./nanocodex.wasm",external:true}));
        b.onResolve({filter:/^\.\/whatsapp-runtime$/},()=>({path:root+"test/whatsapp/runtime.fixture.ts"}));
      }}]}),
  ]);
  const { readFile } = await import("node:fs/promises");
  const trace = [], brokerRequests = [];
  let fault;
  const mf = new Miniflare({workers:[
    {name:"edge", modules:true, compatibilityDate:"2026-07-29", serviceBindings:{MANAGED:"managed"}, script:`export default {fetch(r,e) { const u=new URL(r.url); if(u.pathname.startsWith("/__connect/")){u.protocol="https:"; u.host="nanocodex.internal"; u.port=""; u.pathname=u.pathname.slice(10); return e.MANAGED.fetch(new Request(u,r));} return e.MANAGED.fetch(r); }}`},
    {name:"managed", script:managed.outputFiles[0].text, modules:true, compatibilityDate:"2026-07-29", compatibilityFlags:["nodejs_compat"],
      durableObjects:Object.fromEntries([["NANOCODEX_AUTH","NonceStorage"],["NANOCODEX_USERS","UserAccount"],
        ["NANOCODEX_ORGANIZATIONS","Organization"],["NANOCODEX_API_KEYS","ApiKeyRecord"]].map(([key,className])=>[key,{className,useSQLite:true}])),
      serviceBindings:{NANOCODEX:async request => {
        brokerRequests.push({path:new URL(request.url).pathname, method:request.method,
          ...(request.method === "POST" ? {body:await request.clone().json()} : {})});
        // Observe the actual shipped broker request before simulating a lost or contaminated response.
        const response = await (await mf.getWorker("broker")).fetch(request);
        if (new URL(request.url).pathname.endsWith("/pairing")) return response;
        if (fault === "lost") throw new Error("TEST-1234 provider exception");
        if (fault === "malformed") return new Response("TEST-1234", {status:202});
        if (fault === "timeout") return Response.json({error:"TEST-1234"},{status:408});
        if (fault === "unavailable") return Response.json({error:"TEST-1234",pairing_code:"TEST-1234"},{status:503});
        if (fault === "conflict") return Response.json({error:"TEST-1234",pairing_code:"TEST-1234"},{status:409});
        const value = await response.json();
        if (fault === "wrong-operation") value.attempt.operation_id = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        if (fault === "unsafe-phase") value.attempt.state = "TEST-1234";
        if (fault === "unsafe-expiry") value.attempt.expires_at = "TEST-1234";
        if (value.connectors?.whatsapp) Object.assign(value.connectors.whatsapp, {pairing_code:"TEST-1234",attempt:{code:"TEST-1234"}});
        // Extra broker fields, even on errors/list, must never reach the model.
        return Response.json({...value, code:"TEST-1234", pairing_code:"TEST-1234", error:"TEST-1234"}, {status:response.status});
      }}},
    {name:"broker", modulesRoot:root+"output", modules:[{type:"ESModule",path:root+"output/native-link-broker.js",contents:broker.outputFiles[0].text},
      {type:"CompiledWasm",path:root+"output/nanocodex.wasm",contents:await readFile(root+"../nanocodex/pkg-web/nanocodex_bg.wasm")}],
      compatibilityDate:"2026-07-29", compatibilityFlags:["nodejs_compat"], bindings:{ENVIRONMENT:"test",ALLOW_LOCAL_CREDENTIAL_CLAIM:"true"},
      durableObjects:Object.fromEntries(Object.entries({USER_CONNECTORS:"UserConnectorBroker",WHATSAPP_ACCOUNTS:"WhatsAppAccount",
        AGENT_SUBJECTS:"AgentSubjectDirectory",USER_CREDENTIALS:"UserCredentialBroker",MCP_CONNECTIONS:"McpConnectionDirectory",
        SPOTIFY_RATE_LIMITS:"SpotifyRateLimit",GMAIL_PUSH_MAILBOXES:"GmailPushMailbox"}).map(([key,className])=>[key,{className,useSQLite:true}])),
      outboundService:()=>new Response("unexpected upstream network",{status:599})},
  ]});
  try {
    const base = await mf.ready;
    const user = "11111111-1111-4111-8111-111111111111", other = "44444444-4444-4444-8444-444444444444";
    const op = "33333333-3333-4333-8333-333333333333";
    const op2 = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA";
    const phone = "+15550000001";
    async function issue(user,capabilities) {
      const r = await fetch(new URL("/__fixture",base),{method:"POST",body:JSON.stringify({user,capabilities})});
      assert.equal(r.status,200); return r.json();
    }
    const owner = await issue(user), foreign = await issue(other), scoped = await issue(user,["tools:use"]);
    const connect = {operation:"connect",connector:"whatsapp",phone,operation_id:op};
    async function tool(name,request,{token=owner.token,subagent=false,headers={},delegated=false,http=200}={}) {
      const response = await fetch(new URL((delegated?"/__connect":"")+"/__fixture/account-connectors",base),{
        method:"POST",headers:{authorization:`Bearer ${token}`,"content-type":"application/json",...headers},
        body:JSON.stringify({request,subagent}),
      });
      const text=await response.text(); assert.ok(!text.includes("TEST-1234"),"pairing material leaked in "+name);
      const result=JSON.parse(text); trace.push({name,expectedHTTP:http,observedHTTP:response.status,result});
      assert.equal(response.status,http,text); return result;
    }
    function hint(value,phase= "ready",operation_id=op) {
      assert.deepEqual(Object.keys(value).sort(),["ok","type","status","connector","agent_id","operation_id","expires_at","phase","message"].sort());
      assert.equal(value.ok,true); assert.equal(value.type,"whatsapp_link"); assert.equal(value.status,"input_required");
      assert.equal(value.connector,"whatsapp"); assert.equal(value.agent_id,"synthetic-session"); assert.equal(value.operation_id,operation_id); assert.equal(value.phase,phase);
      assert.ok(Number.isSafeInteger(value.expires_at) && value.expires_at > 0);
      assert.equal(typeof value.message,"string");
    }
    const missing = await tool("missing phone creates no attempt",{operation:"connect",connector:"whatsapp"});
    assert.equal(missing.status,"input_required"); assert.equal(missing.type,undefined); assert.equal(brokerRequests.length,0);
    for (const request of [
      {...connect,operation_id:undefined}, {...connect,operation_id:"invalid"}, {...connect,phone:"555"},
      {...connect,phone:"+1555extension00001"}, {...connect,phone:"+15550000001;TEST-1234"},
      {...connect,phone:"+"+"1".repeat(65)}, {...connect,phone:15550000001},
      {...connect,connector:"google"}, {operation:"list",operation_id:op},
      {operation:"disconnect",connector:"whatsapp",phone}, {...connect,user_id:other}, {...connect,code:"TEST-1234"},
    ]) await tool("strict phone/operation/control validation",request,{http:400});
    assert.equal(brokerRequests.length,0);
    assert.equal((await tool("scoped key cannot initiate",connect,{token:scoped.token})).status,"forbidden");
    assert.equal((await tool("child agent cannot initiate",connect,{subagent:true})).status,"forbidden");
    const connectHeaders = {authorization:"","x-nanocodex-connect-user":user,"x-nanocodex-connect-grant-id":"0x"+"a".repeat(64),
      "x-nanocodex-connect-capabilities":JSON.stringify(["tools:use"]),
      "x-nanocodex-connect-connectors":"[]","x-nanocodex-connect-mcp-ids":"[]"};
    assert.equal((await tool("Connect grant cannot initiate",connect,{headers:connectHeaders,delegated:true})).status,"forbidden");
    await tool("anonymous cannot initiate",connect,{token:"",http:401});
    assert.equal(brokerRequests.length,0);

    const started=await tool("known phone starts the broker and returns native hint",{...connect,phone:" +1 555-000-0001 "});
    hint(started);
    assert.deepEqual(brokerRequests[0],{path:`/users/${user}/connectors/whatsapp/start`,method:"POST",body:{phone,operation_id:op}});
    const replay=await tool("same operation reconciles",connect); hint(replay); assert.equal(replay.expires_at,started.expires_at);
    const accounts=await mf.getDurableObjectNamespace("WHATSAPP_ACCOUNTS","broker");
    const brokers=await mf.getDurableObjectNamespace("USER_CONNECTORS","broker");
    const fixture=accounts.get(accounts.idFromName(brokers.idFromName(user).toString()));
    async function upstream(path,body) {
      const r=await fixture.fetch("https://fixture/fixture/"+path,{method:body===undefined?"GET":"POST",...(body===undefined?{}:{body:JSON.stringify(body)})});
      assert.equal(r.status,200); return r.json();
    }
    assert.equal((await upstream("stats")).pairingRequests,1);
    assert.equal((await tool("same ID different phone conflicts",{...connect,phone:"+15550000002"})).status,"conflict");
    assert.equal((await tool("new ID cannot replace active attempt",{...connect,operation_id:op2})).status,"conflict");
    for (const mode of ["lost","timeout","malformed","unavailable","wrong-operation","unsafe-phase","unsafe-expiry"]) {
      const before=brokerRequests.length;
      fault=mode; hint(await tool(mode+" preserves unknown operation",connect),"unknown"); fault=undefined;
      assert.equal(brokerRequests.length,before+1);
      assert.deepEqual(brokerRequests.at(-1).body,{phone,operation_id:op});
      hint(await tool(mode+" reconciles with original operation",connect));
    }
    fault="conflict"; assert.equal((await tool("untrusted conflict is fixed and private",connect)).status,"conflict"); fault=undefined;
    assert.equal((await upstream("stats")).pairingRequests,1);
    const inventory=await tool("read-only status never carries code",{operation:"list"});
    assert.equal(inventory.connectors.whatsapp.connected,false);
    assert.equal(brokerRequests.some(r=>r.path.includes("pairing")),false);
    trace.push({name:"tool broker trace excludes private pairing endpoint",requests:brokerRequests.slice()});

    const privateURL=new URL(`/v1/connectors/whatsapp/pairing?operation_id=${op}`,base);
    const own=await fetch(privateURL,{headers:{authorization:`Bearer ${owner.token}`}});
    assert.equal(own.status,200); assert.equal((await own.json()).code,"TEST-1234");
    const denied=await fetch(privateURL,{headers:{authorization:`Bearer ${foreign.token}`}});
    assert.equal(denied.status,404); assert.ok(!(await denied.text()).includes("TEST-1234"));
    trace.push({name:"only owning native API retrieves fixture code",ownerHTTP:200,otherOwnerHTTP:404,code:"[private fixture verified]"});
    const foreignInventory=await tool("other owner list isolated",{operation:"list"},{token:foreign.token});
    assert.equal(foreignInventory.connectors.whatsapp.connected,false);
    await upstream("expire",{});
    hint(await tool("expired receipt remains same operation",connect),"expired");
    hint(await tool("explicit new attempt after expiry",{...connect,operation_id:op2}),"ready",op2);
    await upstream("register",{});
    hint(await tool("paired receipt remains same operation",{...connect,operation_id:op2}),"paired",op2);
    assert.equal((await tool("list verifies completed linking",{operation:"list"})).connectors.whatsapp.connected,true);
    assert.equal((await tool("other owner never sees linked account",{operation:"list"},{token:foreign.token})).connectors.whatsapp.connected,false);
  } finally {
    await mf.dispose();
    const output=new URL("../../../output/whatsapp-public/",import.meta.url);
    await mkdir(output,{recursive:true}); await writeFile(new URL("native-link-trace.json",output),JSON.stringify(trace,null,2));
  }
});
