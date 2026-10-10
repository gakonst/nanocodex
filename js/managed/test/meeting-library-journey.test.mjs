import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { startMeetingFixture, fixtureKeys } from "../scripts/meeting-library-fixture.mjs";
import { fetch } from "./support/miniflare-fetch.mjs";

test("meeting library HTTP journey: durable revisions, isolation, summaries and permanent deletion", {timeout:120000}, async()=>{
 const output=resolve("../../output/meeting-library-journey"),persist=output+"/state",trace=[];
 await rm(persist,{recursive:true,force:true});await mkdir(output,{recursive:true});
 let fixture=await startMeetingFixture({port:0,persist});
 const id=crypto.randomUUID(),second=crypto.randomUUID();
 const source={revision:7,title:"Synthetic launch planning",started_at:"2026-09-30T10:00:00Z",duration_seconds:123,transcript:"We decided to launch on Friday. Ada will prepare the checklist.",notes:"Check launch owner. 🚀",partial:true};
 async function call(path,method="GET",body,key=fixtureKeys.owner,expected=200,headers={}){
  const r=await fetch(new URL("/v1/meetings"+path,fixture.base),{method,headers:{...(key?{authorization:"Bearer "+key}:{}),"content-type":"application/json",...headers},...(body===undefined?{}:{body:JSON.stringify(body)})});
  const raw=await r.text();let data;try{data=JSON.parse(raw)}catch{data=raw}
  trace.push({path,method,principal:Object.keys(fixtureKeys).find(k=>fixtureKeys[k]===key)??(headers.cookie?"account_session":"unauthenticated"),expected,status:r.status,data});
  assert.equal(r.status,expected,method+" "+path+": "+raw);return data;
 }
 async function provider(body){return (await fetch(new URL("/__fixture/provider",fixture.base),{method:body?"POST":"GET",...(body?{body:JSON.stringify(body)}:{})})).json()}
 try{
  await call("","GET",undefined,null,401);await call("","GET",undefined,"ncx_live_invalid",401);
  await call("","GET",undefined,fixtureKeys.connect,403);await call("","GET",undefined,fixtureKeys.readonly,403);
  assert.deepEqual((await call("")).meetings,[]);
  await call("/anything","PUT",source,fixtureKeys.owner,404);await call("/"+id,"POST",source,fixtureKeys.owner,405);
  await call("/"+id,"PUT",{...source,revision:0},fixtureKeys.owner,400);
  await call("/"+id,"PUT",source,null,403,{cookie:"meeting_fixture_session=owner"});
  await call("/"+id,"PUT",source,null,403,{cookie:"meeting_fixture_session=owner",origin:"https://evil.test"});
  let saved=(await call("/"+id,"PUT",source,null,200,{cookie:"meeting_fixture_session=owner",origin:fixture.base.origin})).meeting;
  assert.equal(saved.revision,7);assert.equal(saved.notes,source.notes);assert.equal(saved.partial,true);assert.equal(saved.summary_status,"none");
  assert.equal(saved.started_at,"2026-09-30T10:00:00.000Z");assert.ok(Number.isFinite(Date.parse(saved.updated_at)));
  assert.deepEqual((await call("/"+id,"PUT",source)).meeting,saved);
  await call("/"+id,"PUT",{...source,title:"Conflicting same revision"},fixtureKeys.owner,409);
  await call("/"+id,"PUT",{...source,revision:6},fixtureKeys.owner,409);
  for(const key of [fixtureKeys.other,fixtureKeys.organization,fixtureKeys.team]){
   assert.deepEqual((await call("","GET",undefined,key)).meetings,[]);await call("/"+id,"GET",undefined,key,404);
   await call("/"+id+"/summarize","POST",{revision:7},key,404);
  }
  await call("/"+second,"PUT",{...source,revision:1,started_at:"2026-09-29T10:00:00Z",title:"Older recording"});
  const page=await call("?limit=1");assert.equal(page.meetings[0].id,id);assert.equal(page.meetings[0].transcript,undefined);assert.ok(page.next_cursor);
  const next=await call("?limit=1&cursor="+encodeURIComponent(page.next_cursor));assert.equal(next.meetings[0].id,second);assert.equal(next.next_cursor,null);
  await call("?cursor="+encodeURIComponent(page.next_cursor),"GET",undefined,fixtureKeys.other,400);
  await call("?limit=101","GET",undefined,fixtureKeys.owner,400);await call("?cursor=bad","GET",undefined,fixtureKeys.owner,400);await call("?limit=1&limit=2","GET",undefined,fixtureKeys.owner,400);
  await call("/"+id+"/preview","GET",undefined,fixtureKeys.owner,503);
  const edit={...source,revision:11,notes:"Edited notes",partial:false};saved=(await call("/"+id,"PUT",edit)).meeting;assert.equal(saved.partial,false);
  await fixture.mf.dispose();fixture=await startMeetingFixture({port:0,persist});
  assert.deepEqual((await call("/"+id)).meeting,saved);assert.deepEqual((await call("/"+id,"PUT",edit)).meeting,saved);
  await call("/"+id+"/summarize","POST",{revision:7},fixtureKeys.owner,409);
  const recap=(await call("/"+id+"/summarize","POST",{revision:11})).meeting;
  assert.equal(recap.summary_status,"ready");for(const heading of ["Key points","Decisions","Actions"])assert.ok(recap.summary.includes("## "+heading));
  const calls=(await provider()).calls;assert.equal(calls,1);
  assert.deepEqual((await call("/"+id+"/summarize","POST",{revision:11})).meeting,recap);assert.equal((await provider()).calls,calls);
  await provider({fail:true});const failed=(await call("/"+second+"/summarize","POST",{revision:1})).meeting;
  assert.equal(failed.summary_status,"unavailable");assert.equal(failed.transcript,source.transcript);assert.equal(failed.notes,source.notes);
  const failedCalls=(await provider()).calls;await provider({fail:false});assert.equal((await call("/"+second+"/summarize","POST",{revision:1})).meeting.summary_status,"ready");assert.equal((await provider()).calls,failedCalls+1);
  await call("/"+second,"PUT",{...source,revision:2});await provider({pause:true});
  const concurrent=await Promise.all([call("/"+second+"/summarize","POST",{revision:2}),call("/"+second+"/summarize","POST",{revision:2})]);
  assert.ok(concurrent.some(r=>r.meeting.summary_status==="ready"));assert.equal((await provider()).calls,failedCalls+2);await provider({pause:false});
  await call("/"+second,"PUT",{...source,revision:3});await provider({pause:true});const before=(await provider()).calls;
  const pending=call("/"+second+"/summarize","POST",{revision:3},fixtureKeys.owner,409);
  for(let i=0;i<100&&(await provider()).calls===before;i++)await new Promise(r=>setTimeout(r,10));
  await call("/"+second,"PUT",{...source,revision:4,notes:"New revision during summary"});await pending;
  assert.equal((await call("/"+second)).meeting.summary_status,"none");await provider({pause:false});
  await call("/"+id,"DELETE",undefined,fixtureKeys.other,204);assert.equal((await call("/"+id)).meeting.id,id);
  await call("/"+id,"DELETE",undefined,fixtureKeys.owner,204);await call("/"+id,"GET",undefined,fixtureKeys.owner,404);
  await call("/"+id,"PUT",{...edit,revision:1000},fixtureKeys.owner,410);await call("/"+id,"DELETE",undefined,fixtureKeys.owner,204);
  const never=crypto.randomUUID();await call("/"+never,"DELETE",undefined,fixtureKeys.owner,204);await call("/"+never,"PUT",source,fixtureKeys.owner,410);
  await fixture.mf.dispose();fixture=await startMeetingFixture({port:0,persist});await call("/"+id,"PUT",{...edit,revision:1001},fixtureKeys.owner,410);assert.equal((await call("")).meetings.length,1);
  const large=await fetch(new URL("/v1/meetings/"+crypto.randomUUID(),fixture.base),{method:"PUT",headers:{authorization:"Bearer "+fixtureKeys.owner,"content-type":"application/json"},body:JSON.stringify({...source,transcript:"x".repeat(1024*1024)})});
  trace.push({boundary:"body_size",expected:413,status:large.status,data:await large.json()});assert.equal(large.status,413);
  const streaming=await fetch(new URL("/v1/meetings/"+crypto.randomUUID(),fixture.base),{method:"PUT",duplex:"half",headers:{authorization:"Bearer "+fixtureKeys.owner,"content-type":"application/json"},body:new ReadableStream({start(c){for(let i=0;i<18;i++)c.enqueue(new TextEncoder().encode("x".repeat(65536)));c.close()}})});
  trace.push({boundary:"stream_size",expected:413,status:streaming.status,data:await streaming.json()});assert.equal(streaming.status,413);
  const badType=await fetch(new URL("/v1/meetings/"+second,fixture.base),{method:"PUT",headers:{authorization:"Bearer "+fixtureKeys.owner,"content-type":"text/plain"},body:JSON.stringify(source)});assert.equal(badType.status,415);trace.push({boundary:"media_type",status:badType.status,expected:415});
  const db=await fixture.mf.getD1Database("NANOCODEX_CRM","managed");
  // A second device has many local checkpoints but only knew server revision4.
  await call("/"+second,"PUT",{...source,revision:5,notes:"Device A notes"},fixtureKeys.owner,200,{"if-match":"\"4\""});
  await call("/"+second,"PUT",{...source,revision:20,notes:"Stale device B higher checkpoint"},fixtureKeys.owner,409,{"if-match":"\"4\""});
  assert.equal((await call("/"+second)).meeting.notes,"Device A notes");
  await call("/"+second,"PUT",{...source,revision:5,notes:"Device A notes"},fixtureKeys.owner,200,{"if-match":"\"4\""});
  await call("/"+second,"PUT",{...source,revision:20},fixtureKeys.owner,409,{"if-match":"\"0\""});
  const missing=crypto.randomUUID();await call("/"+missing,"PUT",source,fixtureKeys.owner,409,{"if-match":"\"1\""});
  await call("/"+missing,"PUT",source,fixtureKeys.owner,200,{"if-match":"\"0\""});await call("/"+missing,"DELETE",undefined,fixtureKeys.owner,204);
  // Full source rolling inference must include a decision in the middle, not just head/tail.
  const long=crypto.randomUUID(), longInput={...source,revision:1,transcript:"Start. "+"x".repeat(33000)+" MIDDLE_DECISION_CHECK "+"y".repeat(33000)+" End.",notes:"FINAL_USER_NOTES_CHECK"};
  await call("/"+long,"PUT",longInput);const countBefore=(await provider()).calls;
  assert.equal((await call("/"+long+"/summarize","POST",{revision:1})).meeting.summary_status,"ready");
  const providerView=await provider(), sourceCalls=providerView.requests.slice(countBefore);
  assert.equal(sourceCalls.length,4);
  const sourceParts=sourceCalls.map(r=>r.input.messages.find(m=>m.role==="user").content.split("Next source chunk:\n")[1]).join("");
  assert.equal(sourceParts,`Partial recording: yes\nTranscript:\n${longInput.transcript}\nUser notes:\n${longInput.notes}`);
  trace.push({boundary:"full_source_rolling_summary",chunks:sourceCalls.length,source_bytes:Buffer.byteLength(sourceParts),middle_decision_included:sourceParts.includes("MIDDLE_DECISION_CHECK"),user_notes_included:sourceParts.includes("FINAL_USER_NOTES_CHECK")});
  // Maximum permitted source sizes remain practical: all 764KiB are processed, not rejected at32KiB.
  const maxDoc=crypto.randomUUID(), maxInput={...source,revision:1,transcript:"x".repeat(700*1024),notes:"y".repeat(64*1024)};
  await call("/"+maxDoc,"PUT",maxInput);const maxBefore=(await provider()).calls;
  assert.equal((await call("/"+maxDoc+"/summarize","POST",{revision:1})).meeting.summary_status,"ready");
  const maxCalls=(await provider()).calls-maxBefore;assert.equal(maxCalls,39);
  trace.push({boundary:"maximum_source_full_summary",transcript_bytes:700*1024,notes_bytes:64*1024,chunks:maxCalls,status:"ready"});
  await call("/"+maxDoc,"DELETE",undefined,fixtureKeys.owner,204);
  await call("/"+long,"PUT",{...longInput,revision:2});
  await db.prepare("INSERT INTO meeting_library_summary_budget(owner_id,day,count) VALUES(?,?,119) ON CONFLICT(owner_id,day) DO UPDATE SET count=119").bind("fixture-owner",Math.floor(Date.now()/86400000)).run();
  await call("/"+long+"/summarize","POST",{revision:2},fixtureKeys.owner,429);assert.equal((await call("/"+long)).meeting.summary_status,"none");
  await db.prepare("UPDATE meeting_library_summary_budget SET count=0 WHERE owner_id='fixture-owner'").run();
  // Three known failed attempts are bounded; editing creates a fresh attempt allowance.
  await provider({fail:true});const failBefore=(await provider()).calls;
  for(let i=0;i<4;i++)assert.equal((await call("/"+second+"/summarize","POST",{revision:5})).meeting.summary_status,"unavailable");
  assert.equal((await provider()).calls,failBefore+3);await provider({fail:false});
  await call("/"+second,"PUT",{...source,revision:6});
  // Fixture seeds a prior process' expired claim; restart then real HTTP recovers it.
  await db.prepare("UPDATE meeting_library SET summary_claim_until=?,summary_attempts=1,summary_status='unavailable' WHERE id=? AND owner_id='fixture-owner'").bind(Date.now()-1,second).run();
  await fixture.mf.dispose();fixture=await startMeetingFixture({port:0,persist});
  assert.equal((await call("/"+second+"/summarize","POST",{revision:6})).meeting.summary_status,"ready");
  trace.push({boundary:"expired_crash_lease_after_restart",seeded_prior_claim:true,recovered:true});
  const restartDb=await fixture.mf.getD1Database("NANOCODEX_CRM","managed");
  await restartDb.prepare("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<998) INSERT INTO meeting_library(owner_id,organization_id,team_id,id,title,started_at,updated_at,duration_seconds,transcript,notes,partial,revision,content_hash) SELECT 'fixture-owner','fixture-org','fixture-team',printf('00000000-0000-4000-8000-%012d',i),'Quota fixture','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',0,'','',0,1,'' FROM n").run();
  await call("/"+crypto.randomUUID(),"PUT",source,fixtureKeys.owner,429);
  await call("/"+second,"PUT",{...source,revision:7,notes:"Existing update still allowed at storage quota"});
 }finally{
  await fixture.mf.dispose();await writeFile(output+"/http-trace.json",JSON.stringify(trace,null,2));
  await writeFile(output+"/README.md",`# Meeting library HTTP journey\nCommand: cd js/managed && node --test test/meeting-library-journey.test.mjs\nRuntime: Miniflare/workerd HTTP edge -> shipped account proxy -> shipped meeting router -> durable D1. Only trusted authentication boundary and unavoidable external inference provider are synthetic.\nAssertions: direct authorization/origin, owner/org/team isolation, arbitrary positive first revision, replay/conflict, edit, keyset pagination, persistence after two restarts, ready summary per-revision idempotency, recoverable bounded failure retry, full-source rolling summary including middle decisions and notes, weighted quota and expired crash lease recovery, racing summaries and edits, permanent deletion including unknown UUID, body/media limits, persisted summary/storage quotas and atomic If-Match multi-device conflict/replay.\nTrace: http-trace.json (${trace.length} requests).\n`);
 }
});

test("raw meeting audio HTTP journey: resumable immutable parts, hour-long CAF, isolation and deletion", {timeout:120000}, async()=>{
 const {createHash}=await import("node:crypto"),http=await import("node:http");
 const output=resolve("../../output/meeting-audio-journey"),persist=output+"/state",trace=[];
 await rm(persist,{recursive:true,force:true});await mkdir(output,{recursive:true});
 let fixture=await startMeetingFixture({port:0,persist});
 const id=crypto.randomUUID(),hash=b=>createHash("sha256").update(b).digest("hex"),partSize=8*1024*1024;
 // One hour of 16kHz mono 16-bit PCM: 115MB total, above single-request ingress.
 const size=16000*2*3600+68,header=Buffer.alloc(68);header.write("caff");header.writeUInt16BE(1,4);
 header.write("desc",8);header.writeBigInt64BE(32n,12);header.writeDoubleBE(16000,20);header.write("lpcm",28);
 header.writeUInt32BE(12,32);header.writeUInt32BE(2,36);header.writeUInt32BE(1,40);header.writeUInt32BE(1,44);header.writeUInt32BE(16,48);
 header.write("data",52);header.writeBigInt64BE(BigInt(size-64),56);
 const part=number=>{const bytes=Buffer.alloc(Math.min(partSize,size-(number-1)*partSize),number);if(number===1)header.copy(bytes);return bytes;};
 const count=Math.ceil(size/partSize),digest=createHash("sha256");for(let n=1;n<=count;n++)digest.update(part(n));const sha=digest.digest("hex");
 const source={revision:1,title:"Synthetic hour-long recording",started_at:"2026-09-30T10:00:00Z",duration_seconds:3600,transcript:"Original retained.",notes:"",partial:false};
 async function call(path,method="GET",body,key=fixtureKeys.owner,expected=200,extra={}) {
  const r=await fetch(new URL("/v1/meetings"+path,fixture.base),{method,headers:{...(key?{authorization:"Bearer "+key}:{}),...(body?{"content-type":body instanceof Buffer?"application/octet-stream":"application/json"}:{}),...extra},...(body===undefined?{}:{body:body instanceof Buffer?body:JSON.stringify(body)})});
  const bytes=Buffer.from(await r.arrayBuffer()),data=r.headers.get("content-type")?.includes("json")?JSON.parse(bytes):undefined;
  trace.push({path,method,principal:Object.keys(fixtureKeys).find(k=>fixtureKeys[k]===key)??"session_or_unauthenticated",expected,status:r.status,bytes:bytes.length,...(data?{data}:{})});
  assert.equal(r.status,expected,method+" "+path+": "+(r.status===200?"":bytes));return {r,bytes,data};
 }
 const audio="/"+id+"/audio",start=(key=fixtureKeys.owner,expected=200,extra={})=>call(audio,"POST",{size,sha256:sha},key,expected,extra);
 const put=(number,body=part(number),expected=200,extra={},path=audio)=>call(path+"/parts/"+number,"PUT",body,fixtureKeys.owner,expected,{"x-content-sha256":hash(body),...extra});
 async function downloaded(path,expectedSha,expectedSize){
  const r=await fetch(new URL("/v1/meetings"+path,fixture.base),{headers:{authorization:"Bearer "+fixtureKeys.owner}});assert.equal(r.status,200);
  assert.equal(r.headers.get("content-type"),"application/x-caf");assert.equal(r.headers.get("content-length"),String(expectedSize));assert.equal(r.headers.get("cache-control"),"no-store");assert.equal(r.headers.get("x-content-sha256"),expectedSha);
  let read=0;const digest=createHash("sha256");for await(const chunk of r.body){read+=chunk.length;digest.update(chunk);}assert.equal(read,expectedSize);assert.equal(digest.digest("hex"),expectedSha);
  trace.push({boundary:"streamed_original_download",path,bytes:read,sha256:expectedSha,content_length:r.headers.get("content-length"),status:r.status});
 }
 try{
  await call("/"+id,"PUT",source);await call(audio,"GET",undefined,fixtureKeys.owner,404);
  await start(null,401);await start(fixtureKeys.connect,403);await start(fixtureKeys.readonly,403);
  await start(null,403,{cookie:"meeting_fixture_session=owner"});
  for(const key of [fixtureKeys.other,fixtureKeys.organization,fixtureKeys.team])await start(key,404);
  await call(audio,"POST",{size:2*1024*1024*1024+1,sha256:sha},fixtureKeys.owner,413);
  const admission=(await start()).data;assert.equal(admission.audio.part_size,partSize);assert.equal(admission.audio.count,count);assert.deepEqual(admission.uploaded_parts,[]);
  await call(audio,"POST",{size,sha256:"0".repeat(64)},fixtureKeys.owner,409);
  await put(1,Buffer.alloc(partSize),400);await put(1,part(1),400,{"x-content-sha256":"0".repeat(64)});
  await put(1,header,415,{"content-type":"audio/wav"});
  await put(1);await put(1);const changed=part(1);changed[100]^=1;await put(1,changed,409);
  await call(audio+"/complete","POST",undefined,fixtureKeys.owner,409);await call(audio,"GET",undefined,fixtureKeys.owner,404);
  await fixture.mf.dispose();fixture=await startMeetingFixture({port:0,persist});assert.deepEqual((await start()).data.uploaded_parts,[1]);
  // A device continues after restart; competing immutable writers can only win once.
  const changed2=part(2);changed2[100]^=1;
  const raced=await Promise.all([part(2),part(2)].map(body=>put(2,body)));assert.equal(raced.length,2);await put(2,changed2,409);
  for(let n=3;n<=count;n++)await put(n);
  const completed=(await call(audio+"/complete","POST")).data;assert.equal(completed.complete,true);assert.equal(completed.audio.sha256,sha);
  assert.equal((await start()).data.complete,true);await call(audio+"/complete","POST");
  await downloaded(audio,sha,size);
  for(const key of [fixtureKeys.other,fixtureKeys.organization,fixtureKeys.team])await call(audio,"GET",undefined,key,404);
  await fixture.mf.dispose();fixture=await startMeetingFixture({port:0,persist});await downloaded(audio,sha,size);
  await call("/"+id,"PUT",{...source,revision:2,notes:"Text edits preserve audio."});await downloaded(audio,sha,size);
  const chunked=await fetch(new URL("/v1/meetings"+audio+"/parts/1",fixture.base),{method:"PUT",duplex:"half",headers:{authorization:"Bearer "+fixtureKeys.owner,"content-type":"application/octet-stream","x-content-sha256":sha},body:new ReadableStream({start(c){c.enqueue(header);c.close()}})});assert.equal(chunked.status,411);await chunked.text();trace.push({boundary:"unknown_part_length",expected:411,status:chunked.status});
  const over=await new Promise((resolve,reject)=>{const req=http.request(new URL("/v1/meetings"+audio+"/parts/1",fixture.base),{method:"PUT",headers:{authorization:"Bearer "+fixtureKeys.owner,"content-type":"application/octet-stream","x-content-sha256":sha,"content-length":partSize+1}},r=>{r.resume();r.on("end",()=>resolve(r.statusCode))});req.on("error",reject);req.end(header);});assert.equal(over,413);trace.push({boundary:"part_size_cap",expected:413,status:over});
  // A delete completes while a part body is still arriving. Either admission
  // order must end with a permanent tombstone and no retained part objects.
  const racing=crypto.randomUUID(),racingPath="/"+racing+"/audio",racingBytes=part(1);
  await call("/"+racing,"PUT",source);await call(racingPath,"POST",{size:racingBytes.length,sha256:hash(racingBytes)});
  let release;const tail=new Promise(resolve=>release=resolve);
  const pending=fetch(new URL("/v1/meetings"+racingPath+"/parts/1",fixture.base),{method:"PUT",duplex:"half",headers:{authorization:"Bearer "+fixtureKeys.owner,"content-type":"application/octet-stream","content-length":String(racingBytes.length),"x-content-sha256":hash(racingBytes)},body:new ReadableStream({async start(c){c.enqueue(racingBytes.subarray(0,68));await tail;c.enqueue(racingBytes.subarray(68));c.close();}})});
  await call("/"+racing,"DELETE",undefined,fixtureKeys.owner,204);release();const raceReply=await pending;assert.equal(raceReply.status,410);await raceReply.text();trace.push({boundary:"delete_racing_part_upload",status:410});
  await call("/"+id,"DELETE",undefined,fixtureKeys.other,204);await downloaded(audio,sha,size);
  await call("/"+id,"DELETE",undefined,fixtureKeys.owner,204);await call(audio,"GET",undefined,fixtureKeys.owner,404);await start(fixtureKeys.owner,410);await put(1,part(1),410);
  const bucket=await fixture.mf.getR2Bucket("NANOCODEX_WORKSPACES","managed");assert.equal((await bucket.list()).objects.length,0);
  await fixture.mf.dispose();fixture=await startMeetingFixture({port:0,persist});await start(fixtureKeys.owner,410);
  trace.push({boundary:"physical_audio_deletion_and_persistent_tombstone",remaining_objects:0});
 }finally{
  await fixture.mf.dispose();await writeFile(output+"/http-trace.json",JSON.stringify(trace,null,2));
  await writeFile(output+"/README.md","Command: cd js/managed && node --test test/meeting-library-journey.test.mjs\nReal HTTP -> shipped account proxy -> meeting routes -> persistent D1/R2 in workerd. Synthetic authentication only. An hour-long 16kHz LPCM CAF (115,200,068 bytes) is uploaded in 8MiB parts, resumed after restart, completed with full checksum verification, downloaded across restarts and text edits, and physically deleted. Auth, account/org/team isolation, origin, format/checksum rejection, immutable concurrent writes, unknown/oversized part admission, and delete/upload race asserted. Trace: http-trace.json.\n");
 }
});
