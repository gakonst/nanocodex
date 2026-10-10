import { env, runInDurableObject, createExecutionContext, abortAllDurableObjects, applyD1Migrations } from "cloudflare:test";
import { beforeAll, afterEach, describe, expect, it } from "vitest";
import { ManagedAgentOwnership, type Env, type DurableAgentSession } from "../src/index";
import { ensureAccount } from "../src/account-auth";

// Boundary failure modes: foreign ownership, invalid/unbounded payloads, duplicate
// deliveries (including races/eviction), conflicting replay, and quiet processing while busy.
const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;
const crmBindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
beforeAll(async () => applyD1Migrations(crmBindings.NANOCODEX_CRM, crmBindings.CRM_MIGRATIONS));
const fixtureAgents = new Set<string>();
afterEach(async () => {
  for (const agentId of fixtureAgents) {
    const stub = sessions().getByName(agentId);
    await runInDurableObject(stub, async (_, state) => {
      // Terminate the explicitly busy fixture before teardown.
      state.storage.sql.exec("UPDATE managed_turns SET state='cancelled', retry_at=NULL WHERE state='accepted'");
      await state.storage.deleteAlarm();
    });
  }
  await abortAllDurableObjects();
  fixtureAgents.clear();
});
const input = (agentId: string) => ({ userId: "gmail-fixture-owner", agentId,
  eventId: "gmail:connection:history:123", input: "A new message arrived. Summarize it." });
async function initialize(state: DurableObjectState, agentId: string, session: DurableAgentSession, ownerId = "gmail-fixture-owner") {
  fixtureAgents.add(agentId);
  // A fresh Session creates its schema on its first real request (9d8b63102);
  // this ownerless request is refused but initializes storage before seeding.
  await session.fetch(new Request("https://session.internal/sites"));
  // Gate mandatory credential-subject startup, not optional account discovery.
  // Provider dependencies are synthetic; Gmail processing must never execute a chat turn.
  const current = (session as unknown as {env:Env}).env;
  Object.defineProperty(session,"env",{value:{...current,NANOCODEX:{fetch:async(request:RequestInfo | URL)=>{
    if (new URL(request instanceof Request ? request.url : String(request)).pathname.startsWith("/subjects/")) {
      throw Object.assign(new Error("fixture unavailable"),{code:"retryable"});
    }
    return Response.json({connectors:{},mcp_connections:[]});
  }}}});
  state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
    authorization_epoch,public_origin,runtime_profile,last_active)
    VALUES(1,? ,?,'org','team',1,'https://nanocodex.example','managed',?)`, agentId, ownerId, Date.now());
}

function expectQuiet(state: DurableObjectState) {
  expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toHaveLength(0);
  expect(state.storage.sql.exec("SELECT cursor FROM managed_events").toArray()).toHaveLength(0);
}

describe("private quiet Gmail processing", () => {
  it("exposes the wake only on the private ownership host and validates before dispatch", async () => {
    const entrypoint = new ManagedAgentOwnership(createExecutionContext(), env as unknown as Env);
    const agentId = crypto.randomUUID();
    const url = "https://managed-ownership.internal/v1/gmail-push/wake";
    const request = (target = url, body: unknown = input(agentId)) => new Request(target, {
      method: "POST", headers: {"content-type":"application/json"}, body: JSON.stringify(body),
    });
    expect((await entrypoint.fetch(request(url.replace("managed-ownership.internal", "example.com")))).status).toBe(400);
    expect((await entrypoint.fetch(request(`${url}?extra=true`))).status).toBe(400);
    expect((await entrypoint.fetch(request(url, {...input(agentId), extra: true}))).status).toBe(400);
    expect((await entrypoint.fetch(request())).status).toBe(403);
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => await initialize(state, agentId, session));
    const accepted = await entrypoint.fetch(request());
    expect(accepted.status).toBe(200);
    expect(await accepted.json()).toEqual({status:"accepted"});
    expect(await (await entrypoint.fetch(request())).json()).toMatchObject({status:"duplicate"});
    expect((await entrypoint.fetch(request(url, {...input(agentId),input:"changed"}))).status).toBe(409);
    await abortAllDurableObjects();
    expect(await (await entrypoint.fetch(request())).json()).toEqual({status:"duplicate"});
    expect((await entrypoint.fetch(request(url, {...input(agentId),input:"changed"}))).status).toBe(409);
    await runInDurableObject(sessions().getByName(agentId), async (_, state) => expectQuiet(state));
  });

  it("rejects foreign/missing owners and unbounded payloads without admission", async () => {
    const agentId = crypto.randomUUID();
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      await expect(session.gmailPushWake(input(agentId))).rejects.toThrow("gmail_push_owner_forbidden");
      await initialize(state, agentId, session);
      for (const patch of [{userId:"foreign"}, {agentId:crypto.randomUUID()}]) {
        await expect(session.gmailPushWake({...input(agentId), ...patch})).rejects.toThrow("gmail_push_owner_forbidden");
      }
      for (const patch of [{input:""}, {input:"é".repeat(16_385)}, {eventId:"x".repeat(257)}, {agentId:"bad"}]) {
        await expect(session.gmailPushWake({...input(agentId), ...patch})).rejects.toThrow("invalid_gmail_push_wake");
      }
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toHaveLength(0);
    });
  });

  it("processes quietly while a chat turn is busy and keeps its receipt when idle", async () => {
    const agentId = crypto.randomUUID();
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      await initialize(state, agentId, session);
      state.storage.sql.exec(`INSERT INTO managed_turns(id,request_hash,input_json,authorization_json,state,
        accepted_cursor,created_at,accepted_at,updated_at,retry_at)
        VALUES('busy','hash','"busy"','{"capabilities":[]}','accepted',1,?,?,?,?)`,
      Date.now(),Date.now(),Date.now(),Date.now()+60_000);
      expect(await session.gmailPushWake(input(agentId))).toEqual({status:"accepted"});
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toHaveLength(1);
      state.storage.sql.exec("UPDATE managed_turns SET state='completed' WHERE id='busy'");
      expect(await session.gmailPushWake(input(agentId))).toEqual({status:"duplicate"});
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toEqual([{id:"busy"}]);
      expect(state.storage.sql.exec("SELECT cursor FROM managed_events").toArray()).toHaveLength(0);
    });
  });

  it("replays historical Gmail turn receipts quietly and preserves conflicts", async () => {
    const agentId = crypto.randomUUID();
    const wake = input(agentId);
    const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(JSON.stringify([wake.userId,wake.agentId,wake.eventId]))));
    const id = "gmail:" + Array.from(new Uint8Array(bytes), byte => byte.toString(16).padStart(2,"0")).join("");
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      await initialize(state, agentId, session);
      state.storage.sql.exec(`INSERT INTO managed_turns(id,request_key,request_hash,input_json,authorization_json,state,
        accepted_cursor,created_at,accepted_at,updated_at)
        VALUES(?,?,'legacy-hash',?,'{"capabilities":[]}','completed',1,?,?,?)`,
        id,id,JSON.stringify("Historical Gmail notification instructions.\n\n"+wake.input),Date.now(),Date.now(),Date.now());
      await expect(session.gmailPushWake({...wake,input:"Changed"})).rejects.toThrow("different input");
      expect(await session.gmailPushWake(wake)).toEqual({status:"duplicate"});
      expect(await session.gmailPushWake(wake)).toEqual({status:"duplicate"});
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toEqual([{id}]);
      expect(state.storage.sql.exec("SELECT cursor FROM managed_events").toArray()).toHaveLength(0);
    });
  });

  it("processes a concurrent event once, replays its receipt and rejects changed input", async () => {
    const agentId = crypto.randomUUID();
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      await initialize(state, agentId, session);
      const results = await Promise.all([session.gmailPushWake(input(agentId)),session.gmailPushWake(input(agentId))]);
      expect(results.map(r=>r.status).sort()).toEqual(["accepted","duplicate"]);
      expect(results).toEqual([{status:"accepted"},{status:"duplicate"}]);
      expect(await session.gmailPushWake(input(agentId))).toEqual({status:"duplicate"});
      await expect(session.gmailPushWake({...input(agentId),input:"Changed"})).rejects.toThrow("different input");
      expectQuiet(state);
    });
  });
});

// Opt-in ingestion must complete before acknowledging the event; partial work
// binds the input durably and remains retryable without creating chat events.
it("advances opted-in CRM mail in bounded retries without chat events", async () => {
  const agentId = crypto.randomUUID(); const connectionId = "C".repeat(43);
  await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
    await initialize(state, agentId, session);
    let reads = 0;
    const current = (session as unknown as {env:Env}).env;
    Object.defineProperty(session,"env",{value:{...current,NANOCODEX:{fetch:async(value:RequestInfo | URL,init?:RequestInit)=>{
      const request = value instanceof Request ? value : new Request(value,init);
      if (new URL(request.url).pathname.startsWith("/subjects/")) return new Response(null,{status:204});
      if (new URL(request.url).hostname === "gmail.googleapis.com") {
        reads++;
        expect(request.method).toBe("GET");
        expect(request.headers.get("x-nanocodex-connector-connection")).toBe(connectionId);
        return Response.json({id:new URL(request.url).pathname.split("/").at(-1),internalDate:"1",labelIds:["INBOX"],payload:{headers:[{name:"From",value:"unknown@example.test"}]}});
      }
      return current.NANOCODEX.fetch(request);
    }}}});
    const envelope = {...input(agentId),input:JSON.stringify({connectionId,email:"self@example.test",type:"gmail.history",startHistoryId:"1",historyId:"2",messageIds:Array.from({length:7},(_,i)=>`m${i}`),truncated:false,crm:true})};
    expect(await session.gmailPushWake(envelope)).toEqual({status:"busy",progress:true});
    expect(reads).toBe(5);
    expectQuiet(state);
    await expect(session.gmailPushWake({...envelope,input:JSON.stringify({crm:false})})).rejects.toThrow("different input");
    expect(reads).toBe(5);
    expect(await session.gmailPushWake(envelope)).toMatchObject({status:"accepted"});
    expect(reads).toBe(7);
    expect(await session.gmailPushWake(envelope)).toMatchObject({status:"duplicate"});
    expect(reads).toBe(7);
    expectQuiet(state);
  });
});

it("keeps failed CRM wakes retryable without blocking unrelated quiet notifications", async () => {
  const agentId = crypto.randomUUID();
  await runInDurableObject(sessions().getByName(agentId), async (session,state) => {
    await initialize(state, agentId,session);
    const current = (session as unknown as {env:Env}).env;
    Object.defineProperty(session,"env",{value:{...current,NANOCODEX_CRM:undefined}});
    await expect(session.gmailPushWake({...input(agentId),input:JSON.stringify({crm:true})})).rejects.toThrow("gmail_push_crm_unavailable");
    expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toHaveLength(0);
    await expect(session.gmailPushWake({...input(agentId),input:JSON.stringify({crm:true})})).rejects.toThrow("gmail_push_crm_unavailable");
    await expect(session.gmailPushWake(input(agentId))).rejects.toThrow("different input");
    expect(await session.gmailPushWake({...input(agentId),eventId:"unrelated"})).toEqual({status:"accepted"});
    expectQuiet(state);
  });
});

// TODO publication is retained and serialized independently of chat admission.
it("proposes a single TODO for concurrent reply notifications and stays quiet for non-replies", async () => {
  const agentId = crypto.randomUUID(), userId = crypto.randomUUID();
  await ensureAccount(env as unknown as Env, userId, true);
  await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
    await initialize(state, agentId, session, userId);
    const current = (session as unknown as {env:Env}).env;
    let classifications = 0;
    let choice = "reply_requested";
    Object.defineProperty(session, "env", {value: { ...current, NANOCODEX_FIREHOSE_DECISIONS_OWNER_ID:userId,
      AI: {run:async () => {
        classifications++;
        return {state:"Completed",result:{answers:{action:{choice,confidence:0.95}}}};
      }} }});
    const inputValue = JSON.stringify({connectionId:"fixture-connection",email:"self@example.test",type:"gmail.history",
      messageIds:["m1"],messages:[{id:"m1",status:"ok",headers:{from:"Sender <sender@example.test>",
        subject:"Please reply"},body:"Can you reply to me?"}]});
    const wake = {...input(agentId), userId, input:inputValue};
    expect(await Promise.all([session.gmailPushWake(wake),session.gmailPushWake(wake)]))
      .toEqual([{status:"accepted"},{status:"duplicate"}]);
    expect(classifications).toBe(1);
    expect(await session.gmailPushWake(wake)).toEqual({status:"duplicate"});
    await expect(session.gmailPushWake({...wake,input:inputValue.replace("Please reply", "Changed")})).rejects.toThrow("different input");
    expect(classifications).toBe(1);
    expect(state.storage.sql.exec("SELECT source_key, outcome FROM gmail_firehose_decision_receipts_v2").toArray())
      .toMatchObject([{outcome:"reply"}]);
    choice = "no_reply";
    expect(await session.gmailPushWake({...wake,eventId:"no-reply",input:inputValue.replaceAll('"m1"','"m2"')}))
      .toEqual({status:"accepted"});
    expect(classifications).toBe(2);
    expectQuiet(state);
    const inbox = await (await (env as unknown as Env).NANOCODEX_USERS.getByName(userId).fetch("https://user.internal/todo")).json() as
      {decisions: Array<{title:string;choices:unknown[];source_url:string}>};
    expect(inbox.decisions).toHaveLength(1);
    expect(inbox.decisions[0]).toMatchObject({title:"Reply requested: Please reply",source_url:"https://mail.google.com/"});
    const audit = await (await (env as unknown as Env).NANOCODEX_USERS.getByName(userId)
      .fetch("https://user.internal/todo/traces?limit=5")).json() as {traces: Array<{outcome:string;decision_id:string}>};
    expect(audit.traces).toHaveLength(2);
    expect(audit.traces).toEqual(expect.arrayContaining([
      expect.objectContaining({outcome:"reply",decision_id:expect.any(String)}),
      expect.objectContaining({outcome:"no_reply",decision_id:null}),
    ]));
    expect(JSON.stringify(audit)).not.toContain("Can you reply to me?");
  });
});
