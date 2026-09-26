import { env, runInDurableObject, applyD1Migrations, abortAllDurableObjects } from "cloudflare:test";
import { beforeAll, afterEach, expect, it } from "vitest";
import type { DurableAgentSession, Env } from "../src/index";

// Runtime journey: a resolved Calendar outbox survives an active agent turn,
// becomes a content-bearing turn when idle, and cannot produce a replay turn.
const bindings = env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  NANOCODEX_CRM: D1Database;
  CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1];
};
beforeAll(async () => applyD1Migrations(bindings.NANOCODEX_CRM, bindings.CRM_MIGRATIONS));
afterEach(async () => abortAllDurableObjects());
it("admits the persisted event body once after busy backpressure", async () => {
  const agentId = crypto.randomUUID(), sourceId = crypto.randomUUID();
  const owner = "calendar-materialization-fixture", connection = "K".repeat(43);
  const now = Date.now();
  await bindings.NANOCODEX_CRM.prepare(`INSERT INTO crm_calendar_push_sources
    (id,owner_id,agent_id,connection_id,calendar_id,generation,window_from,window_to,rebuild_at,renew_at,check_at,sync_token,notifications_initialized,dirty)
    VALUES(?,?,?,?,?,?,?,?,?,?,?,?,1,0)`).bind(sourceId,owner,agentId,connection,"primary",crypto.randomUUID(),now-3600000,now+3600000,now+3600000,now+3600000,now+3600000,"fixture-sync").run();
  const content = JSON.stringify({provider:"google_calendar",type:"calendar.event.changed",source:{connectionId:connection,calendarId:"primary",eventId:"fixture-event"},event:{id:"fixture-event",title:"Fixture planning",description:"Resolved body: orchid-47",status:"confirmed",start:{dateTime:"2026-09-26T10:00:00Z"},end:{dateTime:"2026-09-26T10:30:00Z"}},untrusted:true});
  await bindings.NANOCODEX_CRM.prepare("INSERT INTO calendar_notification_outbox(id,source_id,input,created_at) VALUES(?,?,?,?)").bind("fixture-notification",sourceId,content,now).run();
  await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(agentId), async (session,state) => {
    const current = (session as unknown as {env:Env}).env;
    let providerReads = 0;
    Object.defineProperty(session,"env",{value:{...current,NANOCODEX:{fetch:async(value:RequestInfo|URL,init?:RequestInit)=>{
      const request = value instanceof Request ? value : new Request(value,init);
      const url = new URL(request.url);
      if(url.pathname.startsWith("/subjects/")) return new Response(null,{status:204});
      if(url.hostname==="www.googleapis.com") {
        providerReads++;
        expect(request.method).toBe("GET");
        expect(request.headers.get("x-nanocodex-connector-connection")).toBe(connection);
        return Response.json({kind:"calendar#events",items:[],nextSyncToken:"fixture-next"});
      }
      return Response.json({connectors:{},mcp_connections:[]});
    }}}});
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES(1,?,?,'org','team',1,'https://calendar.example','managed',?)`,agentId,owner,now);
    state.storage.sql.exec(`INSERT INTO managed_turns(id,request_hash,input_json,authorization_json,state,accepted_cursor,created_at,accepted_at,updated_at,retry_at)
      VALUES('busy','hash','"busy"','{"capabilities":[]}','accepted',1,?,?,?,?)`,now,now,now,now+60000);
    try {
      expect(await session.calendarPushReconcile(sourceId)).toMatchObject({enabled:true,complete:false});
      expect(providerReads).toBe(0);
      expect(await bindings.NANOCODEX_CRM.prepare("SELECT id FROM calendar_notification_outbox WHERE source_id=?").bind(sourceId).first()).not.toBeNull();
      state.storage.sql.exec("UPDATE managed_turns SET state='completed' WHERE id='busy'");
      expect(await session.calendarPushReconcile(sourceId)).toMatchObject({enabled:true,complete:true});
      const turns=state.storage.sql.exec<{input_json:string}>("SELECT input_json FROM managed_turns WHERE id!='busy'").toArray();
      expect(turns).toHaveLength(1);
      expect(JSON.parse(turns[0].input_json)).toContain(content);
      expect(JSON.parse(turns[0].input_json)).toContain("untrusted");
      expect(await bindings.NANOCODEX_CRM.prepare("SELECT id FROM calendar_notification_outbox WHERE source_id=?").bind(sourceId).first()).toBeNull();
      await session.calendarPushReconcile(sourceId);
      expect(state.storage.sql.exec("SELECT id FROM managed_turns WHERE id!='busy'").toArray()).toHaveLength(1);
    } finally {
      state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL WHERE state='accepted'");
      await state.storage.deleteAlarm();
    }
  });
});
