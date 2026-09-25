import { expect, it } from "vitest";
import { workspacePushTools } from "../src/workspace-push-tools";
import type { Principal } from "../src/account-auth";

// Failure modes: stale owner/epoch, Connect and read-only authority must never
// mutate a watch; arguments cannot choose account/session or inject route paths;
// status/disable must not carry enable options or silently enable CRM.
const connection = "c".repeat(43);
const context = () => ({ sessionId: "runtime-session", callId: "call", parentCallId: "", model: "test", signal: new AbortController().signal });
const full: Principal = { kind: "account_session", userId: "owner", organizationId: "org", teamId: "team", authorizationEpoch: 4, role: "writer", subjectId: "user:owner", credentialId: "tool", capabilities: ["agents:read", "agents:write", "tools:use"] };
function fixture() {
  let principal: Principal | undefined = full;
  const calls: { request: Request; principal: Principal }[] = [];
  const tools = workspacePushTools({ sessionId: "agent-one", ownerId: "owner", authorizationEpoch: 4, origin: "https://example.test", authorization: () => principal,
    request: async (request, trusted) => { calls.push({request, principal: trusted}); return Response.json({enabled: request.method !== "DELETE"}); } });
  return { calls, tools, set: (next: Principal | undefined) => { principal = next; }, run: (name: string, input: unknown, ctx = context()) => tools.find(t => t.name === name)!.handler(input, ctx) as Promise<unknown> };
}
it("dispatches bounded assistant watch operations through exact public managed routes", async () => {
  const f = fixture();
  await f.run("gmail_watch", {operation: "enable", connection_id: connection, email: "person@example.test"});
  expect(f.calls[0].request.url).toBe(`https://example.test/v1/agents/agent-one/gmail-push/${connection}`);
  expect(await f.calls[0].request.json()).toEqual({email: "person@example.test"});
  expect(f.calls[0].principal).toBe(full);
  expect(f.calls[0].request.headers.get("x-nanocodex-user-id")).toBeNull();
  await f.run("gmail_watch", {operation: "enable", connection_id: connection, email: "person@example.test", crm: true});
  expect(await f.calls[1].request.json()).toEqual({email: "person@example.test", crm: true});
  await f.run("calendar_watch", {operation: "enable", connection_id: connection, calendar_id: "other@example.test/#?", crm: true});
  expect(new URL(f.calls[2].request.url).searchParams.get("calendar_id")).toBe("other@example.test/#?");
  expect(await f.calls[2].request.json()).toEqual({crm: true});
  for (const name of ["gmail_watch", "calendar_watch"]) for (const operation of ["status", "disable"]) {
    await f.run(name, {operation, connection_id: connection});
    const request = f.calls.at(-1)!.request;
    expect(request.method).toBe(operation === "status" ? "GET" : "DELETE");
    expect(request.body).toBeNull();
  }
});
it("rejects stale or insufficient authority before dispatch and keeps reads separate", async () => {
  const f = fixture();
  for (const denied of [undefined, {...full, userId: "other"}, {...full, authorizationEpoch: 3}, {...full, kind: "service" as const}, {...full, connectGrant: {} as NonNullable<Principal["connectGrant"]>}, {...full, capabilities: ["agents:write"] as const}]) {
    f.set(denied);
    await expect(f.run("gmail_watch", {operation: "enable", connection_id: connection, email: "person@example.test"})).rejects.toThrow(/authorization/);
  }
  expect(f.calls).toHaveLength(0);
  f.set({...full, capabilities: ["agents:read", "tools:use"]});
  await f.run("gmail_watch", {operation: "status", connection_id: connection});
  await expect(f.run("calendar_watch", {operation: "disable", connection_id: connection})).rejects.toThrow(/authorization/);
  f.set(full);
  const abort = new AbortController(); abort.abort();
  await expect(f.run("gmail_watch", {operation: "disable", connection_id: connection}, {...context(), signal: abort.signal})).rejects.toThrow();
  expect(f.calls).toHaveLength(1);
});
it("validates tool arguments at runtime, including explicit Calendar CRM opt-in", async () => {
  const f = fixture();
  for (const [name, input] of [
    ["gmail_watch", {operation:"enable", connection_id:connection}],
    ["gmail_watch", {operation:"enable", connection_id:connection, email:"bad"}],
    ["gmail_watch", {operation:"status", connection_id:connection, owner_id:"other"}],
    ["gmail_watch", {operation:"status", connection_id:"../other"}],
    ["gmail_watch", {operation:"disable", connection_id:connection, crm:true}],
    ["calendar_watch", {operation:"enable", connection_id:connection}],
    ["calendar_watch", {operation:"enable", connection_id:connection, crm:false}],
    ["calendar_watch", {operation:"status", connection_id:connection, calendar_id:"\n"}],
  ] as const) await expect(f.run(name, input)).rejects.toThrow();
  expect(f.calls).toHaveLength(0);
});
