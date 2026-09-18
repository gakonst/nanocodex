import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { conversationProjectMigration, migrationAuthorized, migrationOwner, migrationTeam } from "../src/conversation-project-migration";
import type { Principal } from "../src/account-auth";
const ids = [1,2,3,4].map(n => `${n}1111111-1111-4111-8111-111111111111`);
const names = ["Nanocodex","DJBooth","Personal Life"];
const request = (body?:unknown) => new Request("https://user.internal/conversation-project-migration-20260918", body ? {method:"POST",body:JSON.stringify(body)} : {});
async function inside(test:(storage:DurableObjectStorage, registry:{fetch(request:Request):Promise<Response>})=>Promise<void>) {
  const ns = (env as unknown as {NANOCODEX_USERS:DurableObjectNamespace}).NANOCODEX_USERS;
  await runInDurableObject(ns.getByName(crypto.randomUUID()),async (instance,state)=> {
    for(const id of ids) state.storage.sql.exec("INSERT INTO agent_registry(id,title,created_at,updated_at) VALUES (?,?,1,1)",id,id);
    state.storage.sql.exec("INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at) VALUES (?,?,?,?,?,?,?,1)",ids[3]!,ids[0]!,ids[0]!,"active-origin","active-turn","Active child","a".repeat(64));
    await test(state.storage,instance as unknown as {fetch(request:Request):Promise<Response>});
  });
}
async function plan(storage:DurableObjectStorage) {
  const current = await (await conversationProjectMigration(request(),storage)).json<{snapshot_hash:string}>();
  return {expected_snapshot:current.snapshot_hash,assignments:ids.map((id,i)=>({agent_id:id,project_root_id:ids[i===3?0:i]!,project_name:names[i===3?0:i]!}))};
}
describe("one-time account conversation organization",()=> {
  it("requires exact account, team, ordinary auth and capabilities",()=> {
    const p: Principal = {kind:"api_key",userId:migrationOwner,teamId:migrationTeam,organizationId:crypto.randomUUID(),role:"owner",subjectId:`user:${migrationOwner}`,credentialId:"test",authorizationEpoch:1,capabilities:["agents:read","agents:write","tools:use"]};
    expect(migrationAuthorized(p)).toBe(true);
    expect(migrationAuthorized(undefined)).toBe(false);
    for(const override of [{userId:ids[0]},{teamId:ids[0]},{kind:"service"},{kind:"connect_grant"},{capabilities:["agents:read"]},{connectGrant:{}}])
      expect(migrationAuthorized({...p,...override} as Principal)).toBe(false);
  });
  it("backs up all original membership atomically, keeps coordination and supports exact retry",()=>inside(async (storage,registry)=> {
    const before = await (await conversationProjectMigration(request(),storage)).json<any>();
    const input = await plan(storage);
    const response = await conversationProjectMigration(request(input),storage);
    expect(response.status).toBe(201);
    expect(await response.json()).toMatchObject({total:4,counts:{Nanocodex:2,DJBooth:1,"Personal Life":1},execution_links_preserved:1,transcript_mutations:0});
    const after = await (await conversationProjectMigration(request(),storage)).json<any>();
    expect(after.original_membership).toEqual(before.snapshot);
    expect(after.snapshot.execution_links).toEqual(before.snapshot.execution_links);
    expect(after.snapshot.agents).toEqual(before.snapshot.agents);
    expect(after.snapshot.assignments).toHaveLength(4);
    const roster = await (await registry.fetch(new Request("https://user.internal/agents"))).json<any[]>();
    expect(roster.find(a=>a.id===ids[0])).toMatchObject({title:"Nanocodex",projectName:"Nanocodex",projectRootId:ids[0]});
    expect(roster.find(a=>a.id===ids[3])).toMatchObject({projectName:"Nanocodex",parentAgentId:ids[0],originTurnId:"active-origin",projectTurnId:"active-turn"});
    const execution = await (await registry.fetch(new Request(`https://user.internal/project-threads/${ids[0]}`))).json<any>();
    expect(execution.data).toEqual(before.snapshot.execution_links);

    expect((await conversationProjectMigration(request(input),storage)).status).toBe(200);
    input.assignments[3]!.project_root_id=ids[1]!;input.assignments[3]!.project_name=names[1]!;
    expect((await conversationProjectMigration(request(input),storage)).status).toBe(409);
  }));
  it("rejects stale, incomplete, duplicate and foreign inventories without writing",()=>inside(async storage=> {
    const input = await plan(storage);
    for(const assignments of [input.assignments.slice(0,3),[...input.assignments,input.assignments[0]],input.assignments.map((a,i)=>i===3?{...a,agent_id:crypto.randomUUID()}:a)]) {
      expect((await conversationProjectMigration(request({...input,assignments}),storage)).status).toBe(409);
    }
    storage.sql.exec("UPDATE agent_registry SET title='Changed' WHERE id=?",ids[0]!);
    expect((await conversationProjectMigration(request(input),storage)).status).toBe(409);
    expect(storage.sql.exec("SELECT * FROM conversation_projects").toArray()).toEqual([]);
    expect(storage.sql.exec("SELECT * FROM conversation_project_migration").toArray()).toEqual([]);
  }));
  it("rejects inconsistent roots and fewer than three projects",()=>inside(async storage=> {
    const input=await plan(storage);
    input.assignments[0]!.project_root_id=ids[1]!;
    expect((await conversationProjectMigration(request(input),storage)).status).toBe(400);
    for(const a of input.assignments) {a.project_root_id=ids[0]!;a.project_name="Nanocodex";}
    expect((await conversationProjectMigration(request(input),storage)).status).toBe(400);
  }));
});
