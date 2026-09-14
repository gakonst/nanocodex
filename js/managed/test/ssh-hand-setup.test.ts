import { expect, it } from "vitest";
import { serverHandID, serverHandTool, SERVER_HAND_INSTALL } from "../src/ssh-hand-setup";

const owner = "11111111-1111-4111-8111-111111111111";
const identity = { reference: "lab", hostname: "lab.example.com", port: 2222, username: "deploy", host_key_sha256: "SHA256:" + "a".repeat(43) };
const credential = "s".repeat(43);
const context = () => ({ callId: "call", parentCallId: "", sessionId: "session", model: "test", signal: new AbortController().signal });

async function fixture(options: { denied?: boolean; installExit?: number; image?: string; revokeFails?: boolean; lockBusy?: boolean } = {}) {
  const id = await serverHandID(owner, identity.reference);
  const calls: { path: string; method: string; body: any }[] = [];
  let sshCount = 0;
  const egress = { async fetch(input: string, init?: RequestInit) {
    const path = new URL(input).pathname;
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    calls.push({ path, method: init?.method ?? "GET", body });
    if (path.endsWith("/credentials")) return Response.json({ ssh: [identity], private_fixture: "must-not-be-projected" });
    if (path === "/v1/execute") {
      sshCount += 1;
      return Response.json({ exit_code: sshCount === 2 ? options.installExit ?? 0 : 0,
        stdout: credential, stderr: "untrusted server output" });
    }
    throw new Error("Unexpected egress");
  } } as unknown as Fetcher;
  const hosts = { async fetch(input: string, init?: RequestInit) {
    const path = new URL(input).pathname, method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    calls.push({ path, method, body });
    if (path.includes("/hand-host-setups/")) return new Response(null, { status: options.lockBusy && method === "POST" ? 409 : 204 });
    if (path === "/hands/screens") return Response.json({ surfaces: [{ machine_id: `server:${id}`, id: "desktop" }] });
    if (method === "PUT") return Response.json({ credential }, { status: 201 });
    if (method === "DELETE") return new Response(null, { status: options.revokeFails ? 503 : 204 });
    throw new Error("Unexpected management request");
  } } as unknown as Fetcher;
  const tool = serverHandTool({ owner, subject: "x".repeat(64), origin: "https://managed.example",
    image: options.image ?? "registry.example/hand@sha256:" + "a".repeat(64), egress, hosts,
    authorize() { if (options.denied) throw new Error("forbidden"); } });
  return { id, calls, tool };
}

it("projects SSH metadata without other vault fields and checks authority first", async () => {
  const f = await fixture();
  expect(await f.tool.handler({ operation: "list" }, context())).toEqual({ targets: [identity], installation_available: true });
  const denied = await fixture({ denied: true });
  await expect(denied.tool.handler({ operation: "connect", identity_ref: "lab" }, context())).rejects.toThrow("forbidden");
  expect(denied.calls).toEqual([]);
});

it("keeps installation authority out of argv and results, and waits for publication", async () => {
  const f = await fixture();
  const result = await f.tool.handler({ operation: "connect", identity_ref: "lab" }, context());
  expect(result).toEqual({ machine_id: `server:${f.id}`, status: "published", workspace_retained: true });
  const install = f.calls.find(call => call.path === "/v1/execute" && call.body.stdin);
  expect(install?.body).toMatchObject({ identity_ref: "lab", hostname: identity.hostname, port: 2222, username: "deploy", stdin: credential + "\n" });
  expect(install?.body.command[2]).toBe(SERVER_HAND_INSTALL);
  expect(JSON.stringify(install?.body.command)).not.toContain(credential);
  expect(JSON.stringify(result)).not.toContain(credential);
  expect(f.calls.at(-1)).toMatchObject({ path: `/hand-host-setups/${f.id}`, method: "DELETE" });
  expect(f.calls.some(call => call.path === "/hands/screens")).toBe(true);
});

it("revokes failed installation and reports when revocation is unconfirmed", async () => {
  const f = await fixture({ installExit: 73 });
  await expect(f.tool.handler({ operation: "connect", identity_ref: "lab" }, context())).rejects.toThrow("exit 73");
  expect(f.calls.some(call => call.path === `/hand-hosts/${f.id}` && call.method === "DELETE")).toBe(true);
  const failedCleanup = await fixture({ installExit: 73, revokeFails: true });
  await expect(failedCleanup.tool.handler({ operation: "connect", identity_ref: "lab" }, context())).rejects.toThrow("revocation could not be confirmed");
});

it("refuses concurrent setup or an unpinned image without changing the server", async () => {
  for (const options of [{ lockBusy: true }, { image: "registry.example/hand:latest" }]) {
    const f = await fixture(options);
    await expect(f.tool.handler({ operation: "connect", identity_ref: "lab" }, context())).rejects.toThrow();
    expect(f.calls.some(call => call.path === "/v1/execute")).toBe(false);
    expect(f.calls.some(call => call.path.includes("/hand-hosts/") && call.method === "PUT")).toBe(false);
  }
});

it("uses a stable account-local machine identity and revokes before stopping", async () => {
  expect(await serverHandID(owner, "lab")).toBe(await serverHandID(owner, "lab"));
  expect(await serverHandID(owner, "lab")).not.toBe(await serverHandID("another-owner", "lab"));
  const f = await fixture();
  expect(await f.tool.handler({ operation: "disconnect", identity_ref: "lab" }, context())).toMatchObject({ status: "revoked", service_stopped: true });
  const revoke = f.calls.findIndex(call => call.path === `/hand-hosts/${f.id}` && call.method === "DELETE");
  const stop = f.calls.findIndex(call => call.path === "/v1/execute");
  expect(revoke).toBeLessThan(stop);
});
