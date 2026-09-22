import { env, runInDurableObject, evictDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";

it("cold-start retires legacy delegation before replay while keeping ordinary work", async () => {
  const ns = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  const stub = ns.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (_session, state) => {
    // No session owner: this fixture isolates cold-start migration from provider calls.
    state.storage.sql.exec(`CREATE TABLE project_thread_runs (id TEXT PRIMARY KEY);
      CREATE TABLE project_spawn_plans (id TEXT PRIMARY KEY);
      INSERT INTO project_thread_runs VALUES ('legacy');`);
    state.storage.sql.exec("INSERT INTO managed_configuration VALUES (1,?)", JSON.stringify({ tools: ["spawn_project_thread", "update_plan"] }));
    for (const id of ["project-result:legacy", "user-turn"]) {
      state.storage.sql.exec(`INSERT INTO managed_turns
        (id,request_hash,input_json,authorization_json,state,accepted_cursor,may_have_inner_operation,attempt_count,created_at,accepted_at,updated_at)
        VALUES (?,'hash','"preserved input"','{"capabilities":["agents:read","agents:write","tools:use"]}','accepted',0,0,0,1,1,1)`, id);
    }
  });
  await evictDurableObject(stub);
  await runInDurableObject(stub, async (_session, state) => {
    expect(state.storage.sql.exec("SELECT id,state,input_json FROM managed_turns ORDER BY id").toArray()).toEqual([
      { id: "project-result:legacy", state: "cancelling", input_json: '"preserved input"' },
      { id: "user-turn", state: "accepted", input_json: '"preserved input"' },
    ]);
    expect(state.storage.sql.exec("SELECT name FROM sqlite_master WHERE name IN ('project_thread_runs','project_spawn_plans')").toArray()).toEqual([]);
    const row = state.storage.sql.exec<{ body: string }>("SELECT body FROM managed_configuration").one();
    expect(JSON.parse(row.body)).toEqual({ tools: ["update_plan"] });
    await state.storage.deleteAlarm();
  });
});
