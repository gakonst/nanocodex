import { env, runInDurableObject, createExecutionContext, applyD1Migrations, runDurableObjectAlarm } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import worker, { type DurableAgentSession, type Env } from "../src/index";
import type { Principal } from "../src/account-auth";
// Public route must not turn a foreign/Connect grant or implicit watch request
// into CRM collection. These cases fail before provider or D1 access.
it("requires authenticated ownership and explicit CRM opt-in for Calendar push", async () => {
  const agentId = crypto.randomUUID();
  const principal: Principal = {kind:"api_key",userId:"11111111-1111-4111-8111-111111111111",organizationId:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",teamId:"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",authorizationEpoch:1,role:"owner",subjectId:"user:11111111-1111-4111-8111-111111111111",credentialId:"test",capabilities:["agents:read","agents:write","tools:use"]};
  const sessions = (env as unknown as {NANOCODEX_SESSIONS:DurableObjectNamespace<DurableAgentSession>}).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(agentId), async (_,state) => {
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://nanocodex.example','managed',?)`,agentId,principal.userId,principal.organizationId,principal.teamId,Date.now());
  });
  const call = (actor=principal, body:unknown={crm:true}) => worker.fetch(new Request(`https://nanocodex.example/v1/agents/${agentId}/calendar-push/${"C".repeat(43)}`, {method:"PUT",headers:{"content-type":"application/json"},body:JSON.stringify(body)}),env as unknown as Env,createExecutionContext(),actor);
  expect((await call({...principal,userId:"foreign"})).status).toBe(404);
  expect((await call({...principal,kind:"connect_grant"})).status).toBe(403);
  expect((await call({...principal,capabilities:[]})).status).toBe(403);
  expect((await call(principal,{crm:true,padding:"x".repeat(4096)})).status).toBe(413);
  expect((await call(principal,{})).status).toBe(400);
  expect((await call(principal,{crm:true,ownerId:"foreign"})).status).toBe(400);
});

it("delivers a real callback through the alarm and owner-scoped egress into CRM", async () => {
  const db=(env as unknown as {NANOCODEX_CRM:D1Database}).NANOCODEX_CRM;
  await applyD1Migrations(db,(env as unknown as {CRM_MIGRATIONS:Parameters<typeof applyD1Migrations>[1]}).CRM_MIGRATIONS);
  const clock=vi.spyOn(Date,"now").mockReturnValue(Date.now()+3600000);
  const agentId=crypto.randomUUID(), userId=crypto.randomUUID();
  const principal:Principal={kind:"api_key",userId,organizationId:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",teamId:"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",authorizationEpoch:1,role:"owner",subjectId:`user:${userId}`,credentialId:"test",capabilities:["agents:read","agents:write","tools:use"]};
  const sessions=(env as unknown as {NANOCODEX_SESSIONS:DurableObjectNamespace<DurableAgentSession>}).NANOCODEX_SESSIONS;
  let watch:any, cancelled=false, failRenewal=false, changedPages=0;
  const requests:Request[]=[]; let bound=false;
  await runInDurableObject(sessions.getByName(agentId),async(session,state)=>{
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://nanocodex.example','managed',?)`,agentId,userId,principal.organizationId,principal.teamId,Date.now());
    const current=(session as unknown as {env:Env}).env;
    Object.defineProperty(session,"env",{value:{...current,NANOCODEX:{fetch:async(value:RequestInfo|URL,init?:RequestInit)=>{
      const request=value instanceof Request?value:new Request(value,init);
      if(new URL(request.url).pathname.startsWith("/subjects/")){bound=true;expect(await request.json()).toEqual({user_id:userId});return new Response(null,{status:204});}
      expect(bound).toBe(true);
      requests.push(request);
      if(request.url.endsWith("/watch")){if(failRenewal)return new Response(null,{status:403});watch=await request.json();return Response.json({id:watch.id,resourceId:"synthetic-resource",expiration:String(Date.now()+86400000)});}
      return Response.json({items:[cancelled?{id:"recurrence_20260925T100000Z",status:"cancelled",updated:new Date(Date.now()+1000).toISOString()}:{id:"recurrence_20260925T100000Z",recurringEventId:"recurrence",summary:"Synthetic Review",updated:new Date(Date.now()-1000).toISOString(),start:{dateTime:new Date(Date.now()+3600000).toISOString()},end:{dateTime:new Date(Date.now()+7200000).toISOString()},attendees:[{email:"synthetic@example.test"}]}],...(cancelled && ++changedPages <= 6 ? {nextPageToken:`changed-page-${changedPages}`} : {nextSyncToken:cancelled?"next-token":"initial-token"})});
    }}}});
  });
  const endpoint=`https://nanocodex.example/v1/agents/${agentId}/calendar-push/${"C".repeat(43)}`;
  const configured=await worker.fetch(new Request(endpoint,{method:"PUT",headers:{"content-type":"application/json"},body:JSON.stringify({crm:true})}),env as unknown as Env,createExecutionContext(),principal);
  expect(configured.status).toBe(200);
  const {id}=await configured.json() as {id:string};
  const delivery=(env as unknown as Env).NANOCODEX_CALENDAR_PUSH!.getByName(id);
  await runDurableObjectAlarm(delivery);
  expect(await db.prepare("SELECT status FROM crm_meetings WHERE owner_id=?").bind(userId).first()).toEqual({status:"confirmed"});
  cancelled=true; failRenewal=true;
  await db.prepare("UPDATE crm_calendar_push_sources SET renew_at=0 WHERE id=?").bind(id).run();
  const callback=await worker.fetch(new Request("https://nanocodex.example/v1/calendar-push/callback",{method:"POST",headers:{"x-goog-channel-id":watch.id,"x-goog-channel-token":watch.token,"x-goog-resource-id":"synthetic-resource","x-goog-resource-state":"exists","x-goog-message-number":"3"}}),env as unknown as Env,createExecutionContext());
  expect(callback.status).toBe(204);
  await runDurableObjectAlarm(delivery);
  expect(await db.prepare("SELECT status FROM crm_meetings WHERE owner_id=?").bind(userId).first()).toEqual({status:"cancelled"});
  const watchAttempts = () => requests.filter(r=>r.url.endsWith("/watch")).length;
  expect(changedPages).toBe(5);
  expect(watchAttempts()).toBe(2);
  const failedAt = Date.now();
  expect(await db.prepare("SELECT renew_at FROM crm_calendar_push_sources WHERE id=?").bind(id).first()).toEqual({renew_at:expect.any(Number)});
  const retry = await db.prepare("SELECT renew_at FROM crm_calendar_push_sources WHERE id=?").bind(id).first<{renew_at:number}>();
  expect(retry!.renew_at).toBeGreaterThanOrEqual(failedAt+60000);
  clock.mockReturnValue(failedAt+1000);
  await runDurableObjectAlarm(delivery);
  expect(changedPages).toBe(7);
  expect(watchAttempts()).toBe(2);
  expect(await db.prepare("SELECT last_error FROM crm_calendar_push_sources WHERE id=?").bind(id).first()).toEqual({last_error:"watch_renewal_failed"});
  expect(requests.every(r=>r.headers.get("x-nanocodex-connector-connection")==="C".repeat(43) && !!r.headers.get("x-nanocodex-subject"))).toBe(true);
  expect(new URL(requests.at(-1)!.url).searchParams.get("syncToken")).toBe("initial-token");
  expect(await db.prepare("SELECT last_error FROM crm_calendar_push_sources WHERE id=?").bind(id).first()).toEqual({last_error:"watch_renewal_failed"});
  clock.mockReturnValue(retry!.renew_at);
  failRenewal=false;
  await runDurableObjectAlarm(delivery);
  expect(watchAttempts()).toBe(3);
  expect(await db.prepare("SELECT last_error FROM crm_calendar_push_sources WHERE id=?").bind(id).first()).toEqual({last_error:null});
  expect((await worker.fetch(new Request(endpoint,{method:"DELETE"}),env as unknown as Env,createExecutionContext(),principal)).status).toBe(200);
  await runDurableObjectAlarm(delivery);
  await runInDurableObject(delivery,async(_,state)=>expect(await state.storage.getAlarm()).toBeNull());
  clock.mockRestore();
});
