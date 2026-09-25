import { env, runInDurableObject, createExecutionContext } from "cloudflare:test";
import { expect, it } from "vitest";
import worker, { type DurableAgentSession, type Env } from "../src/index";
import type { Principal } from "../src/account-auth";

// Unauthorized watches, confused route ownership, oversized payloads and Connect
// grants must fail before any provider-side configuration is requested.
it("checks account ownership and bounded configuration before enabling Gmail push", async () => {
  const agentId = crypto.randomUUID();
  const principal: Principal = {kind:"api_key",userId:"11111111-1111-4111-8111-111111111111",organizationId:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",teamId:"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    authorizationEpoch:1,role:"owner",subjectId:"user:11111111-1111-4111-8111-111111111111",credentialId:"test",
    capabilities:["agents:read","agents:write","tools:use"]};
  const calls: {url:string;method:string;body:unknown}[] = [];
  let configuredAgent: string | undefined;
  const sessions = (env as unknown as {NANOCODEX_SESSIONS:DurableObjectNamespace<DurableAgentSession>}).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(agentId), async (session,state) => {
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
      authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES(1,?,'11111111-1111-4111-8111-111111111111','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',1,'https://nanocodex.example','managed',?)`,agentId,Date.now());
    const current = (session as unknown as {env:Env}).env;
    Object.defineProperty(session,"env",{value:{...current,NANOCODEX:{fetch:async(request:Request)=>{
      calls.push({url:request.url,method:request.method,body:request.method!=="GET"?await request.json():null});
      return Response.json(request.method==="GET"?{enabled:!!configuredAgent,agentId:configuredAgent}:{enabled:request.method==="PUT"});
    }}}});
  });
  const call = (method:string, actor=principal, body:unknown={email:"fixture@example.com"}, origin?:string) => worker.fetch(
    new Request(`https://nanocodex.example/v1/agents/${agentId}/gmail-push/connection-1`,{
      method,headers:{"content-type":"application/json",...(origin?{origin}:{})},...(method==="PUT"?{body:JSON.stringify(body)}:{})}),
    env as unknown as Env,createExecutionContext(),actor);
  expect((await call("PUT",{...principal,userId:"foreign"})).status).toBe(404);
  expect((await call("PUT",{...principal,kind:"connect_grant"})).status).toBe(403);
  expect((await call("PUT",{...principal,capabilities:[]})).status).toBe(403);
  expect((await call("PUT",{...principal,kind:"account_session"},undefined,"https://foreign.example")).status).toBe(403);
  expect((await call("PUT",principal,{email:"x".repeat(3000)})).status).toBe(413);
  expect((await call("PUT",principal,{email:"fixture@example.com",userId:"foreign"})).status).toBe(400);
  expect(calls).toHaveLength(0);
  expect((await call("PUT")).status).toBe(200);
  expect(calls.at(-1)).toEqual({url:"https://egress.internal/users/11111111-1111-4111-8111-111111111111/gmail-push/connection-1",method:"PUT",
    body:{email:"fixture@example.com",agentId}});
  configuredAgent=crypto.randomUUID();
  expect((await call("GET")).status).toBe(404);
  expect((await call("DELETE")).status).toBe(404);
  expect(calls.filter(c=>c.method==="DELETE")).toHaveLength(0);
  configuredAgent=agentId;
  expect((await call("DELETE")).status).toBe(200);
  expect(calls.at(-1)?.body).toEqual({agentId});
});
