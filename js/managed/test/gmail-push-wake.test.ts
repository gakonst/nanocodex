import { env, runInDurableObject, createExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ManagedAgentOwnership, type Env, type DurableAgentSession } from "../src/index";

// Boundary failure modes: foreign ownership, invalid/unbounded payloads, duplicate
// deliveries (including races), conflicting replay, and active-turn backpressure.
const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;
const input = (agentId: string) => ({ userId: "gmail-fixture-owner", agentId,
  eventId: "gmail:connection:history:123", input: "A new message arrived. Summarize it." });
function initialize(state: DurableObjectState, agentId: string, session: DurableAgentSession) {
  // Keep model execution at its durable retry boundary: this suite owns admission.
  const current = (session as unknown as {env:Env}).env;
  Object.defineProperty(session,"env",{value:{...current,NANOCODEX_ACCOUNT_TOOLS:{getByName:()=>{
    throw Object.assign(new Error("fixture unavailable"),{code:"retryable"});
  }}}});
  state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
    authorization_epoch,public_origin,runtime_profile,last_active)
    VALUES(1,?,'gmail-fixture-owner','org','team',1,'https://nanocodex.example','managed',?)`, agentId, Date.now());
}

describe("private Gmail wake admission", () => {
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
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => initialize(state, agentId, session));
    const accepted = await entrypoint.fetch(request());
    expect(accepted.status).toBe(200);
    expect(await accepted.json()).toMatchObject({status:"accepted",turnId:expect.any(String)});
    expect(await (await entrypoint.fetch(request())).json()).toMatchObject({status:"duplicate"});
    expect((await entrypoint.fetch(request(url, {...input(agentId),input:"changed"}))).status).toBe(409);
  });

  it("rejects foreign/missing owners and unbounded payloads without admission", async () => {
    const agentId = crypto.randomUUID();
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      await expect(session.gmailPushWake(input(agentId))).rejects.toThrow("gmail_push_owner_forbidden");
      initialize(state, agentId, session);
      for (const patch of [{userId:"foreign"}, {agentId:crypto.randomUUID()}]) {
        await expect(session.gmailPushWake({...input(agentId), ...patch})).rejects.toThrow("gmail_push_owner_forbidden");
      }
      for (const patch of [{input:""}, {input:"é".repeat(16_385)}, {eventId:"x".repeat(257)}, {agentId:"bad"}]) {
        await expect(session.gmailPushWake({...input(agentId), ...patch})).rejects.toThrow("invalid_gmail_push_wake");
      }
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toHaveLength(0);
    });
  });

  it("does not enqueue while busy and allows the same event to retry when idle", async () => {
    const agentId = crypto.randomUUID();
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      initialize(state, agentId, session);
      state.storage.sql.exec(`INSERT INTO managed_turns(id,request_hash,input_json,authorization_json,state,
        accepted_cursor,created_at,accepted_at,updated_at,retry_at)
        VALUES('busy','hash','"busy"','{"capabilities":[]}','accepted',1,?,?,?,?)`,
      Date.now(),Date.now(),Date.now(),Date.now()+60_000);
      expect(await session.gmailPushWake(input(agentId))).toEqual({status:"busy"});
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toHaveLength(1);
      state.storage.sql.exec("UPDATE managed_turns SET state='completed' WHERE id='busy'");
      expect(await session.gmailPushWake(input(agentId))).toMatchObject({status:"accepted",turnId:expect.any(String)});
    });
  });

  it("admits a concurrent event once, replays its receipt and rejects changed input", async () => {
    const agentId = crypto.randomUUID();
    await runInDurableObject(sessions().getByName(agentId), async (session, state) => {
      initialize(state, agentId, session);
      const results = await Promise.all([session.gmailPushWake(input(agentId)),session.gmailPushWake(input(agentId))]);
      expect(results.map(r=>r.status).sort()).toEqual(["accepted","duplicate"]);
      expect(results[0].turnId).toBe(results[1].turnId);
      expect(await session.gmailPushWake(input(agentId))).toMatchObject({status:"duplicate",turnId:results[0].turnId});
      await expect(session.gmailPushWake({...input(agentId),input:"Changed"})).rejects.toThrow("different input");
      const rows = state.storage.sql.exec<{input_json:string}>("SELECT input_json FROM managed_turns").toArray();
      expect(rows).toHaveLength(1);
      expect(JSON.parse(rows[0].input_json)).toContain(input(agentId).input);
    });
  });
});
