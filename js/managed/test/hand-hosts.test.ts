import { createExecutionContext, env } from "cloudflare:test";
import { expect, it } from "vitest";
import worker from "../src/index";
import type { Principal } from "../src/account-auth";
import type { AccountHostedTools } from "../src/account-hosted-tools";

function fixture() {
  const principal: Principal = { kind: "api_key", userId: crypto.randomUUID(),
    organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "owner",
    subjectId: "api_key:test", credentialId: "test", authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"] };
  const origin = "https://nanocodex.example";
  const id = crypto.randomUUID();
  const call = (path: string, init: RequestInit = {}, actor?: Principal) => worker.fetch(
    new Request(origin + path, init), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  const manage = (method = "PUT", actor = principal) => call(`/v1/account/hand-hosts/${id}`, {
    method, ...(method === "PUT" ? { body: JSON.stringify({ name: "SSH test server" }) } : {}),
  }, actor);
  return { principal, id, call, manage, origin };
}

function next(socket: WebSocket): Promise<any> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("Missing Hand message")), 2000);
    socket.addEventListener("message", event => { clearTimeout(timer); resolve(JSON.parse(String(event.data))); }, { once: true });
  });
}

it("enrolls a machine-scoped publisher, fences its catalog, and revokes live viewers", async () => {
  const f = fixture();
  const enrolled = await f.manage();
  expect(enrolled.status).toBe(201);
  expect(enrolled.headers.get("cache-control")).toBe("no-store");
  const receipt = await enrolled.json<any>();
  expect(receipt).toMatchObject({ id: f.id, machine_id: `server:${f.id}` });
  const base = new URL(receipt.url).pathname;
  const headers = { authorization: `Bearer ${receipt.credential}` };
  expect((await f.call(base + "/ice", { method: "POST", headers })).status).toBe(200);
  expect((await f.call(base.replace(f.principal.userId, crypto.randomUUID()) + "/ice", { method: "POST", headers })).status).not.toBe(200);
  expect((await f.call(base + "/ice", { method: "POST", headers: { authorization: "Bearer " + "x".repeat(43) } })).status).toBe(401);
  expect((await f.call("/v1/account/hands/screens", { headers })).status).toBe(401);
  const open = async () => {
    const response = await f.call(base + "/host", { headers: { ...headers, upgrade: "websocket" } });
    expect(response.status).toBe(101);
    const socket = response.webSocket!, ready = next(socket); socket.accept();
    return { socket, state: await ready };
  };
  const wrong = await open();
  const wrongClosed = new Promise<CloseEvent>(resolve => wrong.socket.addEventListener("close", resolve, { once: true }));
  const surface = { id: "desktop", name: "Desktop", kind: "desktop", width: 1600, height: 900, controllable: true };
  wrong.socket.send(JSON.stringify({ type: "catalog", machine_id: "another-host", machine_name: "Wrong", surfaces: [surface] }));
  expect((await wrongClosed).code).toBe(1008);
  const host = await open();
  const published = next(host.socket);
  host.socket.send(JSON.stringify({ type: "catalog", machine_id: receipt.machine_id, machine_name: receipt.name, surfaces: [surface] }));
  await published;
  const listing = await (await f.call("/v1/account/hand-hosts", {}, f.principal)).json<any>();
  expect(listing.data).toHaveLength(1);
  expect(JSON.stringify(listing)).not.toContain(receipt.credential);
  expect(JSON.stringify(listing)).not.toContain("tokenDigest");
  const joined = next(host.socket);
  const viewerResponse = await f.call(`/v1/account/hands/view?machine_id=${receipt.machine_id}&surface_id=desktop&generation=${host.state.generation}`,
    { headers: { upgrade: "websocket" } }, f.principal);
  expect(viewerResponse.status).toBe(101);
  const viewer = viewerResponse.webSocket!, ready = next(viewer); viewer.accept(); await ready;
  const closed = new Promise<CloseEvent>(resolve => viewer.addEventListener("close", resolve, { once: true }));
  expect((await joined).type).toBe("viewer");
  const renewal = next(host.socket);
  expect((await f.call(base + "/renew", { method: "POST", headers, body: JSON.stringify({ connection_id: host.state.connection_id }) })).status).toBe(200);
  expect((await renewal).type).toBe("renewed");
  expect((await f.manage("DELETE")).status).toBe(204);
  expect((await closed).code).toBe(1008);
  expect((await f.call(base + "/ice", { method: "POST", headers })).status).toBe(401);
});

it("requires account authority and rotates a stable machine credential on re-enrollment", async () => {
  const f = fixture();
  expect((await f.manage("PUT", { ...f.principal, capabilities: ["agents:read"] })).status).toBe(403);
  expect((await f.manage("PUT", { ...f.principal, connectGrant: { grantId: "test" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
  const first = await (await f.manage()).json<any>();
  const second = await (await f.manage()).json<any>();
  expect(second.machine_id).toBe(first.machine_id);
  expect(second.credential).not.toBe(first.credential);
  const base = new URL(first.url).pathname;
  expect((await f.call(base + "/ice", { method: "POST", headers: { authorization: `Bearer ${first.credential}` } })).status).toBe(401);
  expect((await f.call(base + "/ice", { method: "POST", headers: { authorization: `Bearer ${second.credential}` } })).status).toBe(200);
  expect((await f.call(base + "/ice?unexpected=1", { method: "POST", headers: { authorization: `Bearer ${second.credential}` } })).status).not.toBe(200);
  expect((await f.call(`/v1/account/hand-hosts/${crypto.randomUUID()}`, { method: "PUT", body: JSON.stringify({ name: "name", machine_id: "another" }) }, f.principal)).status).toBe(400);
});

it("serializes setup for one server and rejects another operation's unlock", async () => {
  const owner = crypto.randomUUID(), id = crypto.randomUUID();
  const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
  const stub = namespace.getByName(owner);
  const first = crypto.randomUUID(), second = crypto.randomUUID();
  const call = (method: string, operation: string, actor = owner) => stub.fetch(`https://account-tools.internal/hand-host-setups/${id}`, {
    method, headers: { "x-nanocodex-owner-id": actor }, body: JSON.stringify({ operation_id: operation }),
  });
  expect((await call("POST", first)).status).toBe(204);
  expect((await call("POST", second)).status).toBe(409);
  expect((await call("DELETE", second)).status).toBe(204);
  expect((await call("POST", second)).status).toBe(409);
  expect((await call("DELETE", first, crypto.randomUUID())).status).toBe(404);
  expect((await call("DELETE", first)).status).toBe(204);
  expect((await call("POST", second)).status).toBe(204);
  expect((await call("DELETE", second)).status).toBe(204);
});
