import { expect, it, vi } from "vitest";
import { env } from "cloudflare:test";
import { SandboxDesktop, type SandboxDesktopScope } from "../src/sandbox-desktop";
import { handleSandboxEgress } from "../src/sandbox-runtime";
import type { AccountHostedTools } from "../src/account-hosted-tools";

const scope: SandboxDesktopScope = { owner: "11111111-1111-4111-8111-111111111111", id: "22222222-2222-4222-8222-222222222222", machineId: "cf:mount-test", name: "Cloudflare desktop" };
function fixture() {
  const values = new Map<string, unknown>();
  const storage = { get: async (key: string) => structuredClone(values.get(key)), put: async (key: string, value: unknown) => { values.set(key, structuredClone(value)); }, delete: async (key: string) => values.delete(key), transaction: async (callback: (value: unknown) => unknown) => callback(storage) } as unknown as DurableObjectStorage;
  const runtime = { exec: vi.fn(async () => ({ success: true })), writeFile: vi.fn(async () => ({ success: true })), getProcess: vi.fn(async () => null as null | { status: string }), startProcess: vi.fn(async () => ({})), killProcess: vi.fn(async () => ({})) };
  const fetch = vi.fn(async (_url: string, init: RequestInit) => init.method === "DELETE" ? new Response(null, { status: 204 }) : Response.json({ credential: "a".repeat(43), expires_at: Date.now() + 90 * 86400000 }, { status: 201 }));
  const hosts = { getByName: vi.fn(() => ({ fetch })) } as unknown as DurableObjectNamespace;
  const bind = vi.fn(async () => {});
  const desktop = new SandboxDesktop(storage, runtime as never, hosts, bind);
  return { values, runtime, fetch, hosts, bind, desktop };
}

it("starts after binding, keeps credentials out of argv, reuses a running process and restarts a slept container", async () => {
  const f = fixture();
  await f.desktop.configure(scope);
  expect(f.bind).toHaveBeenCalledWith(scope);
  expect(f.runtime.writeFile).toHaveBeenCalledWith("/run/nanocodex-hand/credential.next", "a".repeat(43) + "\n");
  const command = f.runtime.startProcess.mock.calls[0] as unknown as [string, unknown];
  expect(command[0]).toContain("'server-host' '--frames'");
  expect(command[0]).not.toContain("a".repeat(43));
  expect(command[0]).toContain("'cf:mount-test'");
  f.runtime.getProcess.mockResolvedValue({ status: "running" });
  await f.desktop.configure(scope);
  expect(f.runtime.startProcess).toHaveBeenCalledTimes(1);
  expect(f.fetch).toHaveBeenCalledTimes(1);
  f.runtime.getProcess.mockResolvedValue(null);
  await f.desktop.configure(scope);
  expect(f.runtime.startProcess).toHaveBeenCalledTimes(2);
  expect(f.fetch).toHaveBeenCalledTimes(1);
  await f.desktop.clear();
  expect(f.fetch.mock.calls.at(-1)?.[1].method).toBe("DELETE");
  expect(f.runtime.killProcess).toHaveBeenCalledTimes(1);
  expect(f.values.size).toBe(0);
});

it("fences mount ownership, coalesces starts and refuses an unavailable image before enrollment", async () => {
  const f = fixture();
  await Promise.all([f.desktop.configure(scope), f.desktop.configure(scope)]);
  expect(f.runtime.startProcess).toHaveBeenCalledTimes(1);
  await expect(f.desktop.configure({ ...scope, owner: crypto.randomUUID() })).rejects.toThrow("another mount");
  const broken = fixture();
  broken.runtime.exec.mockResolvedValue({ success: false });
  await expect(broken.desktop.configure(scope)).rejects.toThrow("image is unavailable");
  expect(broken.fetch).not.toHaveBeenCalled();
});

it("retains revocation state on failure and retries removal before killing its process", async () => {
  const f = fixture();
  await f.desktop.configure(scope);
  f.fetch.mockResolvedValueOnce(new Response(null, { status: 503 }));
  await expect(f.desktop.clear()).rejects.toThrow("revoke");
  expect(f.values.size).toBe(1);
  expect(f.runtime.killProcess).not.toHaveBeenCalled();
  await f.desktop.clear();
  expect(f.values.size).toBe(0);
});

it("routes only the trusted sandbox publisher and strips caller routing assertions", async () => {
  const requests: Request[] = [];
  const fetch = vi.fn(async (request: Request) => { requests.push(request); return new Response("routed"); });
  const getByName = vi.fn(() => ({ fetch }));
  const broker = { fetch: vi.fn() } as unknown as Fetcher;
  const env = { NANOCODEX: broker, NANOCODEX_ACCOUNT_TOOLS: { getByName } as unknown as DurableObjectNamespace };
  const params = { hand: { owner: scope.owner, id: scope.id } };
  const base = `https://nanocodex-hand.internal/v1/hand-hosts/${scope.owner}/${scope.id}/hands`;
  const response = await handleSandboxEgress(new Request(base + "/host", { headers: { upgrade: "websocket", authorization: "Bearer scoped-token", "x-nanocodex-owner-id": "forged", "x-nanocodex-subject": "forged" } }), env, { params });
  expect(await response.text()).toBe("routed");
  expect(getByName).toHaveBeenCalledWith(scope.owner);
  expect(requests[0]!.headers.get("x-nanocodex-owner-id")).toBe(scope.owner);
  expect(requests[0]!.headers.get("x-nanocodex-subject")).toBeNull();
  expect(requests[0]!.headers.get("authorization")).toBe("Bearer scoped-token");
  for (const url of [base.replace(scope.owner, crypto.randomUUID()) + "/host", base.replace(scope.id, crypto.randomUUID()) + "/host", base + "/host?other=1", base + "/ice", base.replace("https:", "http:") + "/host"]) {
    expect((await handleSandboxEgress(new Request(url, { headers: { upgrade: "websocket" } }), env, { params })).status).toBe(403);
  }
  expect((await handleSandboxEgress(new Request(base + "/host", { headers: { upgrade: "websocket" } }), env)).status).toBe(403);
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(broker.fetch).not.toHaveBeenCalled();
});

it("enrolls and publishes a Cloudflare surface through the real account broker and intercepted route", async () => {
  const owner = crypto.randomUUID(), id = crypto.randomUUID();
  const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
  const account = namespace.getByName(owner), headers = { "x-nanocodex-owner-id": owner };
  const enrolled = await account.fetch(`https://account-tools.internal/sandbox-hand-hosts/${id}`, { method: "PUT", headers, body: JSON.stringify({ name: "Sandbox", machine_id: "cf:runtime-fixture" }) });
  expect(enrolled.status).toBe(201);
  const receipt = await enrolled.json<{ credential: string }>();
  const response = await handleSandboxEgress(new Request(`https://nanocodex-hand.internal/v1/hand-hosts/${owner}/${id}/hands/host`, { headers: { upgrade: "websocket", authorization: `Bearer ${receipt.credential}` } }), { NANOCODEX: { fetch: vi.fn() } as unknown as Fetcher, NANOCODEX_ACCOUNT_TOOLS: namespace }, { params: { hand: { owner, id } } });
  expect(response.status).toBe(101);
  const host = response.webSocket!;
  const next = () => new Promise<any>(resolve => host.addEventListener("message", event => resolve(JSON.parse(String(event.data))), { once: true }));
  const ready = next(); host.accept(); await ready;
  const published = next();
  host.send(JSON.stringify({ type: "catalog", machine_id: "cf:runtime-fixture", machine_name: "Sandbox", surfaces: [{ id: "desktop", kind: "desktop", name: "Desktop", transport: "frames-v1", width: 1600, height: 900, controllable: true }] }));
  expect((await published).type).toBe("published");
  const catalog = await (await account.fetch("https://account-tools.internal/hands/screens", { headers })).json<{ surfaces: unknown[] }>();
  expect(catalog.surfaces).toMatchObject([{ machine_id: "cf:runtime-fixture", transport: "frames-v1" }]);
  expect((await account.fetch(`https://account-tools.internal/sandbox-hand-hosts/${id}`, { method: "DELETE", headers })).status).toBe(204);
});
