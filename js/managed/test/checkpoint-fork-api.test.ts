import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker, { type Env } from "../src/index";
import type { Principal } from "../src/account-auth";

const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", authorizationEpoch: 1,
  role: "owner", subjectId: "user:11111111-1111-4111-8111-111111111111",
  credentialId: "test", capabilities: ["agents:read", "agents:write", "tools:use"],
};
const parent = "01997777-7777-7777-8777-777777777777";
const snapshot = {version: 1, model: "gpt-6-astra", lineage_id: "lineage", prompt_cache_key: "cache",
  workspace: "/brain", canonical_context: {type: "message", role: "user", content: []}, history: []};
const settings = {model: "gpt-6-astra", thinking: "low", reasoning_mode: "standard", fast_mode: false};
function fixture(parentStatus = 200) {
  const calls: Array<{id:string;path:string;body:string}> = [];
  const seeded = new Map<string, string>();
  const runtime = {...env, NANOCODEX_SESSIONS: {
    idFromName: () => ({toString: () => "a".repeat(64)}),
    getByName: (id:string) => ({fetch: async (input: RequestInfo | URL, init?:RequestInit) => {
      const req = new Request(input, init); const path = new URL(req.url).pathname;
      const body = await req.text(); calls.push({id,path,body});
      if (id === parent && path === "/fork/snapshot") return parentStatus === 200
        ? Response.json({snapshot, settings}) : Response.json({error:"checkpoint_unavailable"},{status:parentStatus});
      if (path === "/fork/status") {
        const row = seeded.get(id);
        if (!row) return Response.json({error:"not_found"},{status:404});
        const value = JSON.parse(row);
        return Response.json({parent_agent_id:value.parent_agent_id,request_key:value.request_key,settings});
      }
      if (path === "/create") return Response.json({prepare_ms:1,initialize_ms:1,commit_ms:1});
      if (path === "/fork/seed") {
        const current = seeded.get(id);
        if (current && current !== body) return Response.json({error:"fork_seed_conflict"},{status:409});
        seeded.set(id, body); return Response.json({seeded:true});
      }
      return Response.json({error:"not_found"},{status:404});
    }})
  }} as unknown as Env;
  const request = (actor:Principal = principal, key = "fork:stable") => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${parent}/forks`, {
      method:"POST", headers:{"idempotency-key":key},
    }), runtime, createExecutionContext(), actor);
  return {calls, seeded, request};
}

describe("checkpoint fork ingress", () => {
  it("creates one independently seeded agent without sending typed history or snapshot to the client", async () => {
    const {calls, seeded, request} = fixture();
    const first = await request(); expect(first.status).toBe(201);
    const body = await first.json() as {agent_id:string; parent_agent_id:string};
    expect(body.parent_agent_id).toBe(parent);
    expect(body.agent_id).not.toBe(parent);
    expect(JSON.stringify(body)).not.toContain("canonical_context");
    expect(JSON.stringify(body)).not.toContain("lineage_id");
    const seed = JSON.parse(seeded.get(body.agent_id)!);
    expect(seed.snapshot).toEqual(snapshot);
    expect(seed.parent_agent_id).toBe(parent);
    expect(calls.filter(c=>c.id===parent).map(c=>c.path)).toEqual(["/fork/snapshot"]);
    expect(calls.some(c=>c.path.includes("turns"))).toBe(false);
    const second = await request(); expect(second.status).toBe(201);
    expect((await second.json() as {agent_id:string}).agent_id).toBe(body.agent_id);
    expect(seeded.size).toBe(1);
  });
  it("fails closed when the checkpoint is unavailable or scope lacks tool authority", async () => {
    const busy = fixture(409);
    expect((await busy.request()).status).toBe(409);
    expect(busy.calls.every(c=>c.id===parent || c.path==="/fork/status")).toBe(true);
    const limited = fixture();
    expect((await limited.request({...principal,capabilities:["agents:read","agents:write"]})).status).toBe(403);
    expect(limited.calls).toHaveLength(0);
    expect((await limited.request(principal, "bad key")).status).toBe(400);
    expect(limited.calls).toHaveLength(0);
  });
});
