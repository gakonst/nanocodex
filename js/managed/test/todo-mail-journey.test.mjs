import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// Real HTTP -> shipped account proxy -> real API-key auth -> shipped TODO router ->
// real account SQLite DO -> credential egress. Only Google/broker is synthetic.
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
    const input = await request.json(); await ensureAccount(env, input.user, true);
    if (input.decision) return Response.json(await env.NANOCODEX_USERS.getByName(input.user).proposeTodoDecision(input.decision));
    const auth = await (await env.NANOCODEX_USERS.getByName(input.user).fetch("https://user.internal/authorization")).json();
    const principal = {kind:"api_key",userId:input.user,...auth.grant,subjectId:"api_key:"+input.user,credentialId:"fixture",capabilities:input.read_only?["agents:read"]:auth.grant.capabilities};
    if (input.session) {
      const token = "s_" + "D".repeat(43);
      await Kv.durableObject(env.NANOCODEX_AUTH, {name:"account"}).set("session:"+token, {userId:input.user,authentication:"sms_otp",issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
      return Response.json({cookie:"nanocodex_account="+token});
    }
    return Response.json(await createApiKey(env, principal, "synthetic-mail-journey"));
  }
  return await routeTodoRequest(request, env, url, await authenticate(request, env, url)) ?? new Response("not_found", {status:404});
} };
`;
const c1 = "A".repeat(43), c2 = "B".repeat(43), unknownConnection = "C".repeat(43);
const msg = (connectionID, html = false) => ({ id: "m1", threadId: "t1", internalDate: "1780000000000", labelIds: ["INBOX", "UNREAD"],
  payload: { mimeType: "multipart/mixed", headers: [{ name: "From", value: "Person <person@example.test>" }, { name: "To", value: "owner@example.test" },
    { name: "Subject", value: connectionID === c1 ? "Review the proposal" : "Other mailbox" }, { name: "Message-ID", value: "<original@example.test>" }], parts: [
    { mimeType: html ? "text/html" : "text/plain", body: { data: Buffer.from(html ? '<p>Hello</p><img src="https://tracker.test/pixel"><script>alert(1)</script><p>&lt;safe&gt;</p>' : "Please review this proposal.\nThanks.").toString("base64url") } },
    { mimeType: "application/pdf", filename: "proposal.pdf", body: { attachmentId: "a1", size: 7 } },
  ] } });

test("TODO mail HTTP journey: multiaccount read, durable review, exact-version send/replay, ambiguous send, and calendar", { timeout: 90_000 }, async () => {
  const trace = [], providerTrace = [], subjects = new Map(); let sends = 0, failSend = false, calendarFailure = false, modifications = [];
  let metadataActive = 0, metadataPeak = 0, threadCount = 1, calendarCount = 2, eventPage = false, metadataInbox = true;
  let calendarActive = 0, calendarPeak = 0, replyHeaders = [], replyThread = "t1", pauseProfile, bodyPart, attachmentResponse;
  const delay = () => new Promise(resolve => setTimeout(resolve, 12));
  const broker = async request => {
    const url = new URL(request.url), connectionID = request.headers.get("x-nanocodex-connector-connection");
    if (url.hostname === "broker.internal") {
      if (url.pathname.startsWith("/subjects/")) { subjects.set(url.pathname.split("/").at(-1), (await request.json()).user_id); return new Response(null, {status:204}); }
      if (url.pathname.endsWith("/connectors")) return Response.json({connectors:{gmail:{connected:true,connections:[c1,c2].map((id,i)=>({id,label:`Mailbox ${i+1}`,capabilities:["gmail","gcalendar"],scopes:["https://www.googleapis.com/auth/gmail.modify"]}))},gcalendar:{connected:true,connections:[{id:c1,label:"Mailbox 1",capabilities:["gmail","gcalendar"]}]}}});
      throw new Error(`Unexpected broker path ${url.pathname}`);
    }
    assert.ok([c1,c2].includes(connectionID), "egress must select exact account");
    assert.ok(subjects.has(request.headers.get("x-nanocodex-subject")), "egress subject bound to owner before provider access");
    assert.equal(request.headers.get("authorization"), "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
    providerTrace.push({method:request.method,path:url.pathname,connection_id:connectionID,query:Object.fromEntries(url.searchParams)});
    if (url.pathname.endsWith("/profile")) { if (pauseProfile) await pauseProfile(); return Response.json({emailAddress: connectionID === c1 ? "owner@example.test" : "other@example.test"}); }
    if (url.pathname.endsWith("/messages/send")) {
      sends++; const body = await request.json(), mime = Buffer.from(body.raw,"base64url").toString();
      assert.match(mime,/To: edited@example.test/); assert.match(mime,/In-Reply-To: <original@example.test>/); assert.equal(body.threadId,"t1");
      assert.ok(mime.includes(Buffer.from("Reviewed body ✓").toString("base64")));
      assert.ok(mime.split("\r\n").every(line => Buffer.byteLength(line) <= 998), "outbound MIME line limit");
      assert.ok([...mime.matchAll(/=\?UTF-8\?B\?[^?]*\?=/g)].every(m => m[0].length <= 75), "encoded words are folded");
      if (failSend) return new Response("Synthetic upstream timeout after acceptance",{status:504});
      return Response.json({id:`sent${sends}`,threadId:"t1"});
    }
    if (url.pathname.endsWith("/threads/t1/modify")) { modifications.push(await request.json()); return Response.json({id:"t1"}); }
    if (url.pathname.endsWith("/threads")) return Response.json({threads:Array.from({length:threadCount},(_,i)=>({id:"t"+(i+1),snippet:"Please review"})),...(url.searchParams.has("pageToken")?{}:{nextPageToken:"page2"})});
    if (url.pathname.endsWith("/threads/deleted")) return new Response("not found",{status:404});
    if (/\/threads\/t\d+$/.test(url.pathname)) {
      const metadata = url.searchParams.get("format") === "metadata";
      if (metadata) { metadataActive++; metadataPeak = Math.max(metadataPeak,metadataActive); await delay(); metadataActive--; }
      const raw=msg(connectionID,url.searchParams.get("format")==="full");if(metadata && !metadataInbox) raw.labelIds=["UNREAD"];
      if(!metadata && bodyPart) raw.payload.parts[0]=bodyPart;
      return Response.json({id:url.pathname.split("/").at(-1),messages:[raw]});
    }
    if (url.pathname.endsWith("/messages/m1/attachments/a1") && attachmentResponse) return attachmentResponse();
    if (url.pathname.endsWith("/messages/m1/attachments/a1")) return Response.json({data:Buffer.from("PDFTEST").toString("base64url"),size:7});
    if (url.pathname.endsWith("/messages/m1")) { const m=msg(connectionID); m.threadId=replyThread; m.payload.headers.push(...replyHeaders); return Response.json(m); }
    if (url.pathname.endsWith("/users/me/calendarList")) return Response.json({items:[{id:"primary",summary:"Work"},...Array.from({length:calendarCount-1},(_,i)=>({id:i===0?"team@example.test":`calendar${i}`,summary:"Team"}))]});
    if (calendarFailure && url.pathname.endsWith("/events") && !url.pathname.includes("primary")) return new Response("Synthetic calendar failure",{status:503});
    if (url.pathname.endsWith("/events")) { calendarActive++; calendarPeak=Math.max(calendarPeak,calendarActive); await delay(); calendarActive--; }
    if (url.pathname.endsWith("/events")) return url.pathname.includes("primary") ? Response.json({...(eventPage?{nextPageToken:"more-events"}:{}),items:[{id:"event1",summary:"Planning",description:"<p>Discuss plans</p>",attendees:[{self:true,responseStatus:"accepted"}],start:{dateTime:"2026-09-29T10:00:00Z"},end:{dateTime:"2026-09-29T11:00:00Z"},htmlLink:"https://calendar.google.com/event?eid=fixture"}]}) : Response.json({items:[{id:"event2",summary:"Team day",start:{date:"2026-09-30"},end:{date:"2026-10-01"}},{id:"declined",summary:"No thanks",attendees:[{self:true,responseStatus:"declined"}],start:{date:"2026-09-30"},end:{date:"2026-10-01"}}]});
    throw new Error(`Unexpected Google path ${url.pathname}`);
  };
  const bundled = await build({stdin:{contents:source,resolveDir:fileURLToPath(new URL("..",import.meta.url))},bundle:true,write:false,format:"esm",target:"es2022",platform:"browser",external:["cloudflare:workers","node:*"],alias:{"node-rsa":"./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"}});
  const script = bundled.outputFiles[0].text;
  const persistence = fileURLToPath(new URL("../../../output/todo-mail-store-"+crypto.randomUUID(),import.meta.url));
  const options = {durableObjectsPersist:persistence,workers:[
    {name:"edge",script,modules:true,compatibilityDate:"2026-07-29",compatibilityFlags:["nodejs_compat"],bindings:{EDGE:true},serviceBindings:{NANOCODEX_BACKEND:"managed"}},
    {name:"managed",script,modules:true,compatibilityDate:"2026-07-29",compatibilityFlags:["nodejs_compat","enable_request_signal"],serviceBindings:{NANOCODEX:broker},durableObjects:{NANOCODEX_AUTH:{className:"NonceStorage",useSQLite:true},NANOCODEX_USERS:{className:"UserAccount",useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:"Organization",useSQLite:true},NANOCODEX_API_KEYS:{className:"ApiKeyRecord",useSQLite:true}}},
  ]};
  let mf = new Miniflare(options);
  try {
    const backend = await mf.getWorker("managed"); let base = await mf.ready;
    async function key(user, read_only=false) { const r=await backend.fetch("https://fixture.test/__fixture",{method:"POST",body:JSON.stringify({user,read_only})});assert.equal(r.status,200,await r.clone().text());return (await r.json()).token; }
    const owner=crypto.randomUUID(), token=await key(owner), other=await key(crypto.randomUUID()), readOnly=await key(owner,true);
    async function call(path, method="GET", body, credential=token, expected=200) {
      const response=await fetch(new URL("/v1/todo"+path,base),{method,headers:{...(credential?{authorization:`Bearer ${credential}`} : {}),"content-type":"application/json"},...(body===undefined?{}:{body:JSON.stringify(body)})});
      const text=await response.text(); let data;try {data=JSON.parse(text);} catch {data=text;}
      trace.push({path,method,status:response.status,data});assert.equal(response.status,expected,`${method} ${path}: ${text}`);return data;
    }
    await call("/mail/accounts","GET",undefined,null,401);
    await call("/mail/accounts","GET",undefined,"ncx_live_invalid",401);
    const sessionResponse = await backend.fetch("https://fixture.test/__fixture",{method:"POST",body:JSON.stringify({user:owner,session:true})});
    const {cookie}=await sessionResponse.json();
    for (const origin of [undefined,"https://unrelated.test",base.origin]) {
      const r = await fetch(new URL("/v1/todo/mail/drafts",base),{method:"POST",headers:{cookie,"content-type":"application/json",...(origin?{origin}:{})},body:"{}"});
      trace.push({path:"/mail/drafts",method:"POST",session_origin:origin,status:r.status,data:await r.json()});assert.equal(r.status,origin===base.origin?400:403);
    }
    const deniedConnect = await backend.fetch("https://nanocodex.internal/v1/todo/mail/accounts",{headers:{"x-nanocodex-connect-user":owner,"x-nanocodex-connect-grant-id":"0x"+"d".repeat(64),"x-nanocodex-connect-capabilities":JSON.stringify(["agents:read"]),"x-nanocodex-connect-connectors":JSON.stringify(["gmail"]),"x-nanocodex-connect-mcp-ids":"[]"}});
    trace.push({path:"/mail/accounts",principal:"connect_grant",status:deniedConnect.status,data:await deniedConnect.json()});assert.equal(deniedConnect.status,403);
    const spoofedConnect = await fetch(new URL("/v1/todo/mail/accounts",base),{headers:{"x-nanocodex-connect-user":owner,"x-nanocodex-connect-grant-id":"0x"+"d".repeat(64),"x-nanocodex-connect-capabilities":JSON.stringify(["agents:read"]),"x-nanocodex-connect-connectors":JSON.stringify(["gmail"]),"x-nanocodex-connect-mcp-ids":"[]"}});
    assert.equal(spoofedConnect.status,401);trace.push({principal:"public_spoofed_connect",status:spoofedConnect.status,data:await spoofedConnect.json()});
    const accounts=await call("/mail/accounts");assert.equal(accounts.accounts.length,2);assert.equal(accounts.accounts[0].email,"owner@example.test");
    const first=await call(`/mail/threads?connection_id=${c1}&q=proposal`);assert.equal(first.threads[0].subject,"Review the proposal");assert.equal(first.next_page_token,"page2");
    assert.equal((await call(`/mail/threads?connection_id=${c1}&q=proposal&page_token=page2`)).next_page_token,null);
    assert.equal((await call(`/mail/threads?connection_id=${c2}`)).threads[0].subject,"Other mailbox");
    threadCount=25;metadataPeak=0;const batch=await call("/mail/threads?connection_id="+c1);assert.equal(batch.threads.length,25);assert.equal(metadataPeak,5);threadCount=1;
    await call(`/mail/threads?connection_id=${unknownConnection}`,"GET",undefined,token,404);
    const summary=(await call("/mail/threads/t1?connection_id="+c1+"&format=metadata")).summary;
    assert.equal(summary.in_inbox,true);assert.equal(summary.message_count,1);assert.equal(summary.subject,"Review the proposal");assert.equal(summary.messages,undefined);
    metadataInbox=false;assert.equal((await call("/mail/threads/t1?connection_id="+c1+"&format=metadata")).summary.in_inbox,false);metadataInbox=true;
    await call("/mail/threads/deleted?connection_id="+c1+"&format=metadata","GET",undefined,token,404);
    const detail=await call(`/mail/threads/t1?connection_id=${c1}`), message=detail.thread.messages[0];
    assert.equal(message.attachments[0].filename,"proposal.pdf");assert.match(message.body_text,/Hello/);assert.match(message.body_html,/&lt;safe&gt;/);assert.doesNotMatch(message.body_html,/<script|<img|tracker\.test/);
    assert.equal(Buffer.from((await call(`/mail/messages/m1/attachments/a1?connection_id=${c1}`)).data,"base64url").toString(),"PDFTEST");
    for (const charset of ["iso-8859-1","windows-1252"]) {
      bodyPart={mimeType:"text/plain",headers:[{name:"Content-Type",value:"text/plain; charset="+charset}],body:{data:Buffer.from([0x63,0x61,0x66,0xe9,0x20,0x80]).toString("base64url")}};
      const decoded=(await call("/mail/threads/t1?connection_id="+c1)).thread.messages[0];
      assert.equal(decoded.body_text,"café €");assert.equal(decoded.body_truncated,false);
    }
    bodyPart={...bodyPart,headers:[{name:"Content-Type",value:"text/plain; charset=x-unknown-charset"}]};
    const unknownCharset=(await call("/mail/threads/t1?connection_id="+c1)).thread.messages[0];assert.equal(unknownCharset.body_text,"");assert.equal(unknownCharset.body_truncated,true);
    bodyPart={mimeType:"text/plain",body:{data:"!invalid base64"}};
    assert.equal((await call("/mail/threads/t1?connection_id="+c1)).thread.messages[0].body_truncated,true);
    bodyPart={mimeType:"text/plain",body:{attachmentId:"a1",size:7}};attachmentResponse=()=>Response.json({size:7});
    assert.equal((await call("/mail/threads/t1?connection_id="+c1)).thread.messages[0].body_truncated,true);
    attachmentResponse=()=>new Response("Missing attachment",{status:404});
    await call("/mail/threads/t1?connection_id="+c1,"GET",undefined,token,404);
    attachmentResponse=()=>Response.json({data:"A".repeat(12*1024*1024+1),size:12*1024*1024});
    await call("/mail/threads/t1?connection_id="+c1,"GET",undefined,token,413);
    bodyPart=undefined;attachmentResponse=undefined;
    const input={id:crypto.randomUUID(),version:0,connection_id:c1,mode:"reply_all",to:["person@example.test"],cc:[],bcc:[],subject:"Re: Review the proposal",body_text:"Draft body",thread_id:"t1",reply_message_id:"m1"};
    await call("/mail/drafts","POST",input,readOnly,403);
    const invalidContent = await fetch(new URL("/v1/todo/mail/drafts",base),{method:"POST",headers:{authorization:"Bearer "+token,"content-type":"text/plain"},body:JSON.stringify(input)});
    assert.equal(invalidContent.status,415);trace.push({boundary:"content_type",status:invalidContent.status,data:await invalidContent.json()});
    await call("/mail/drafts","POST",{...input,to:['bad"address@example.test']},token,400);
    await call("/mail/drafts","POST",{...input,body_text:"\ud800"},token,400);
    await call("/mail/drafts","POST",{...input,raw:"arbitrary MIME"},token,400);
    await call("/mail/drafts","POST",{...input,id:undefined},token,400);
    await call("/mail/drafts","POST",{...input,version:undefined},token,400);
    const saved=(await call("/mail/drafts","POST",input,token,201)).draft;
    assert.equal((await call("/mail/drafts","POST",input)).draft.id,saved.id);
    await call("/mail/drafts","POST",{...input,body_text:"Changed duplicate"},token,409);assert.equal(sends,0,"saving a draft must never send");
    assert.equal((await call(`/mail/drafts/${saved.id}`)).draft.version,1);
    assert.equal((await call(`/mail/drafts?connection_id=${c1}`)).drafts.length,1);
    assert.equal((await call(`/mail/drafts?connection_id=${c1}&thread_id=absent`)).drafts.length,0);
    assert.equal((await call(`/mail/drafts?connection_id=${c1}&thread_id=t1`)).drafts.length,1);
    // A matching older draft remains visible after 101 newer drafts in other threads.
    for (let offset=0;offset<101;offset+=5) await Promise.all(Array.from({length:Math.min(5,101-offset)},(_,i)=>call("/mail/drafts","POST",{...input,id:crypto.randomUUID(),thread_id:"other"+(offset+i)},token,201)));
    assert.equal((await call("/mail/drafts?connection_id="+c1+"&thread_id=t1")).drafts[0].id,saved.id);
    assert.equal((await call("/mail/drafts?connection_id="+c1)).drafts.length,100);
    await call(`/mail/drafts/${saved.id}`,"GET",undefined,other,404);
    await call("/mail/drafts","POST",{...input,subject:"Injected\r\nBcc: attacker@example.test"},token,400);
    const edited=(await call("/mail/drafts","POST",{...input,id:saved.id,version:1,to:["edited@example.test"],body_text:"Reviewed body ✓",subject:"Re: "+"世界".repeat(90)})).draft;
    await call("/mail/drafts","POST",{...input,id:saved.id,version:1},token,409);
    await call("/mail/send","POST",{draft_id:saved.id,version:1,operation_id:crypto.randomUUID()},token,409);assert.equal(sends,0);
    async function decision(source_connection_id,source_message_id) {
      const r=await backend.fetch("https://fixture.test/__fixture",{method:"POST",body:JSON.stringify({user:owner,decision:{source_key:crypto.randomUUID(),title:"Reply",context:"Synthetic decision",source_label:"Gmail",source_url:"https://mail.google.com/",choices:[{id:"follow_up",title:"Follow up"}],source_connection_id,source_thread_id:"t1",source_message_id}})});
      assert.equal(r.status,200,await r.clone().text());return (await r.json()).id;
    }
    const exactDecision=await decision(c1,"m1"),newerDecision=await decision(c1,"m2"),otherDecision=await decision(c2,"m1"),legacyDecision=await decision(null,null);
    const invalidSend={draft_id:edited.id,version:2,operation_id:crypto.randomUUID()};
    replyThread="wrong";await call("/mail/send","POST",invalidSend,token,409);replyThread="t1";
    replyHeaders=[{name:"References",value:"<older@example.test>\r\nBcc: injected@example.test"}];await call("/mail/send","POST",invalidSend,token,400);
    replyHeaders=[{name:"References",value:"arbitrary-header-text"}];await call("/mail/send","POST",invalidSend,token,409);replyHeaders=[{name:"References",value:"<málaga@example.test>"}];await call("/mail/send","POST",invalidSend,token,409);replyHeaders=[];assert.equal(sends,0);
    await call("/mail/send","POST",{...invalidSend,raw:"injected MIME"},token,400);
    const raceInput={...input,id:crypto.randomUUID()};const raceDraft=(await call("/mail/drafts","POST",raceInput,token,201)).draft;
    let reached,release;const reachedProfile=new Promise(r=>reached=r),releaseProfile=new Promise(r=>release=r);
    pauseProfile=async()=>{reached();await releaseProfile;};
    const staleDuringSend=call("/mail/send","POST",{draft_id:raceDraft.id,version:1,operation_id:crypto.randomUUID()},token,409);
    await reachedProfile;await call("/mail/drafts","POST",{...raceInput,version:1,body_text:"New review required"});pauseProfile=undefined;release();await staleDuringSend;assert.equal(sends,0);
    const send={draft_id:edited.id,version:2,operation_id:crypto.randomUUID()};
    const [sent,replayed]=await Promise.all([call("/mail/send","POST",send),call("/mail/send","POST",send)]);
    assert.equal(sends,1,"concurrent send is exactly once");assert.ok([sent,replayed].some(r=>r.receipt.status==="sent"));
    assert.equal((await call("/mail/send","POST",send)).receipt.status,"sent");
    await call("/mail/send","POST",{...send,operation_id:crypto.randomUUID()});assert.equal(sends,1);
    await call("/mail/drafts","POST",{...input,id:saved.id,version:2},token,409);
    const decisions=(await call("")).decisions;
    assert.deepEqual(decisions.filter(d=>d.id===exactDecision).map(d=>[d.status,d.version]),[["resolved",2]]);
    for (const decisionID of [newerDecision,otherDecision,legacyDecision]) assert.deepEqual(decisions.filter(d=>d.id===decisionID).map(d=>[d.status,d.version]),[["needs_you",1]]);
    const unknownDecision=await decision(c1,"m1");
    const ambiguous=(await call("/mail/drafts","POST",{...input,id:crypto.randomUUID(),to:["edited@example.test"],body_text:"Reviewed body ✓"},token,201)).draft;
    failSend=true;const ambiguousSend={draft_id:ambiguous.id,version:1,operation_id:crypto.randomUUID()};
    assert.equal((await call("/mail/send","POST",ambiguousSend)).receipt.status,"unknown");assert.equal(sends,2);
    assert.equal((await call(`/mail/drafts/${ambiguous.id}`)).draft.status,"unknown");
    assert.deepEqual((await call("")).decisions.filter(d=>d.id===unknownDecision).map(d=>[d.status,d.version]),[["needs_you",1]]);
    // Destroy the worker and reopen its persisted SQLite storage before reconciliation.
    await mf.dispose();mf=new Miniflare(options);base=await mf.ready;
    await call("/mail/send","POST",ambiguousSend);await call("/mail/send","POST",{...ambiguousSend,operation_id:crypto.randomUUID()});assert.equal(sends,2,"unknown send is never retried");
    await call("/mail/send","POST",{...ambiguousSend,operation_id:send.operation_id},token,409);
    await call(`/mail/threads/t1/modify`,"POST",{connection_id:c1,archive:true,unread:false});assert.deepEqual(modifications,[{addLabelIds:[],removeLabelIds:["INBOX","UNREAD"]}]);
    await call(`/mail/threads/t1/modify`,"POST",{connection_id:c1,archive:false,unread:true});assert.deepEqual(modifications[1],{addLabelIds:["INBOX","UNREAD"],removeLabelIds:[]});
    const schedule=await call("/schedule?from=2026-09-28T00:00:00Z&to=2026-10-05T00:00:00Z");assert.equal(schedule.events.length,2);assert.equal(schedule.partial,false);assert.equal(schedule.events[1].all_day,true);assert.equal(schedule.events[0].description,"Discuss plans");assert.equal(schedule.events[0].response_status,"accepted");
    calendarFailure=true;const partial=await call("/schedule");assert.equal(partial.partial,true);assert.equal(partial.events.length,1);assert.equal(partial.errors[0].calendar_id,"team@example.test");
    calendarFailure=false;calendarCount=45;calendarPeak=0;eventPage=true;
    const bounded=await call("/schedule");assert.equal(bounded.partial,true);assert.equal(calendarPeak,5);assert.equal(bounded.events.length,40);
    assert.equal(bounded.errors.filter(e=>e.error==="calendar_limit").length,5);assert.equal(bounded.errors.filter(e=>e.error==="event_limit").length,1);
    await call("/schedule?from=bad&to=bad","GET",undefined,token,400);
    await call("/mail/suggest","POST",{connection_id:c1,thread_id:"t1",reply_message_id:"m1"},token,503);
    await call("/mail/suggest","POST",{connection_id:c1,thread_id:"t1",reply_message_id:"m1"},readOnly,403);
    // Enable a synthetic model through the same Workers AI RPC binding contract.
    // Only the external inference is synthetic; shipped suggestion validation and HTTP remain real.
    await mf.dispose();
    options.workers[1].serviceBindings.AI={name:"ai",entrypoint:"SyntheticAI"};
    options.workers.push({name:"ai",modules:true,compatibilityDate:"2026-07-29",script:
      'import {WorkerEntrypoint} from "cloudflare:workers"; export class SyntheticAI extends WorkerEntrypoint { async run(model,input,options) {'+
      'if(input.tools || input.stream!==false || options.gateway.collectLog!==false || !options.gateway.skipCache) throw Error("unsafe inference envelope");'+
      'const context=JSON.parse(input.messages[1].content); if(!context.quoted_messages.length || !context.quoted_messages[0].body.includes("<safe>")) throw Error("missing safe context");'+
      'return {response:JSON.stringify(context.owner_instructions==="malformed"?{body_text:"Unsafe proposal",send:true}:{body_text:"Thanks for the proposal. I will review it."})}; }}'});
    mf=new Miniflare(options);base=await mf.ready;
    const beforeSuggestion=(await call("/mail/drafts?connection_id="+c1+"&thread_id=t1")).drafts;
    const suggestionInput={connection_id:c1,thread_id:"t1",reply_message_id:"m1",instructions:"Keep it brief"};
    assert.deepEqual(await call("/mail/suggest","POST",suggestionInput),{body_text:"Thanks for the proposal. I will review it."});
    await call("/mail/suggest","POST",{...suggestionInput,instructions:"malformed"},token,502);
    await call("/mail/suggest","POST",{...suggestionInput,reply_message_id:"absent"},token,409);
    await call("/mail/suggest","POST",{...suggestionInput,instructions:"x".repeat(2001)},token,400);
    await call("/mail/suggest","POST",{...suggestionInput,to:["injected@example.test"]},token,400);
    assert.deepEqual((await call("/mail/drafts?connection_id="+c1+"&thread_id=t1")).drafts,beforeSuggestion);
    assert.equal(sends,2,"suggestion cannot send mail");
    assert.ok(providerTrace.some(r=>r.query.q==="proposal"));assert.ok(providerTrace.some(r=>r.query.pageToken==="page2"));
    assert.equal(trace.some(r=>JSON.stringify(r.data).includes("NANOCODEX_PROVIDER_CREDENTIAL")),false);
  } finally {
    await mkdir(new URL("../../../output/",import.meta.url),{recursive:true});
    await writeFile(new URL("../../../output/todo-mail-http-journey.json",import.meta.url),JSON.stringify({trace,provider_trace:providerTrace,send_attempts:sends,metadata_peak:metadataPeak,calendar_peak:calendarPeak},null,2));
    await mf.dispose();
    await rm(persistence,{recursive:true,force:true});
  }
});
