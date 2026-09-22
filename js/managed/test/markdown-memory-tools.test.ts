import { env } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import worker from "../src/index";
import { injectMarkdownMemoryBootstrap, markdownMemoryEnabled, markdownMemoryRequest, markdownMemoryTools } from "../src/markdown-memory-tools";
import type { ManagedExtensionOptions } from "../src/extension-tools";

function fixture() {
  let personal = true;
  const fetch = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => Response.json({ content: "saved", revision: 1 }));
  const getByName = vi.fn((_name: string) => ({ fetch }));
  const authorize = vi.fn();
  const options: ManagedExtensionOptions = { organizationId: "org", teamId: "team", ownerId: "alice", sessionId: "session",
    memories: { getByName } as unknown as ManagedExtensionOptions["memories"], authorize, personal: () => personal };
  const context = { sessionId: "session", callId: "call", parentCallId: "", model: "unknown", signal: new AbortController().signal } as ToolContext;
  return { options, context, fetch, getByName, authorize, connect: () => { personal = false; } };
}
it("routes private reads with owner assertions and live authority, and isolates Connect", async () => {
  const f = fixture();
  await markdownMemoryRequest(f.options, "get", { path: "MEMORY.md" }, f.context);
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", "org", "alice"]));
  expect(f.fetch.mock.calls[0]).toEqual(["https://memory.internal/markdown-memory/get", expect.objectContaining({ headers: expect.objectContaining({ "x-nanocodex-private-memory-owner": "alice", "x-nanocodex-team-id": "personal:alice" }) })]);
  f.connect();
  await markdownMemoryRequest(f.options, "search", { query: "saved" }, f.context);
  expect(f.getByName).toHaveBeenLastCalledWith("org");
  await expect(markdownMemoryRequest(f.options, "get", { path: "MEMORY.md", scope: "personal" }, f.context)).rejects.toThrow("direct account");
  f.authorize.mockImplementation(() => { throw new Error("revoked"); });
  await expect(markdownMemoryRequest(f.options, "get", { path: "MEMORY.md" }, f.context)).rejects.toThrow("revoked");
  expect(f.fetch).toHaveBeenCalledTimes(2);
});
it("requires explicit sharing intent and root context before any write reaches storage", async () => {
  const f = fixture();
  const input = { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "shared", scope: "team" };
  await expect(markdownMemoryRequest(f.options, "write", input, f.context)).rejects.toThrow("user's request");
  await expect(markdownMemoryRequest(f.options, "write", { ...input, user_requested: true }, { ...f.context, subagent: {} } as unknown as ToolContext)).rejects.toThrow("root agent");
  expect(f.fetch).not.toHaveBeenCalled();
  await markdownMemoryRequest(f.options, "write", { ...input, user_requested: true }, f.context);
  expect(f.authorize).toHaveBeenLastCalledWith("memory_write", f.context);
  expect(f.fetch.mock.calls[0]).toEqual([expect.any(String), expect.objectContaining({ headers: expect.objectContaining({ "x-nanocodex-memory-mutation": "1" }), body: JSON.stringify({ operation: "put", path: "MEMORY.md", expected_revision: 0, content: "shared" }) })]);
});
it("gates bootstrap by configured reads and exposes distinct Markdown tool names", () => {
  expect(markdownMemoryTools(fixture().options).map(tool => tool.name)).toEqual(["memory_get", "memory_search", "memory_write"]);
  expect(markdownMemoryEnabled()).toBe(true);
  expect(markdownMemoryEnabled(["memory"])).toBe(true);
  expect(markdownMemoryEnabled(["memory_get"])).toBe(true);
  expect(markdownMemoryEnabled(["memory_write"])).toBe(false);
  expect(markdownMemoryEnabled([])).toBe(false);
});
it("protects Markdown API methods and capabilities before forwarding to storage", async () => {
  const token = `ncx_live_${"m".repeat(12)}_${"s".repeat(43)}`;
  const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token))))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = { id: "m".repeat(12), prefix: `ncx_live_${"m".repeat(12)}`, digest, label: "memory", createdAt: 1,
    userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "writer", authorizationEpoch: 1, capabilities: ["memory:read"] };
  const f = fixture();
  const bindings = { ...env, NANOCODEX_MEMORY: f.options.memories, NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: async () => record }) } };
  const call = (path: string, body?: unknown, authenticated = true, method = "POST") => worker.fetch(new Request(`https://test.example/v1/markdown-memory/${path}`, {
    method, headers: { ...(authenticated ? { authorization: `Bearer ${token}` } : {}), "content-type": "application/json" },
    ...(method === "POST" ? { body: JSON.stringify(body) } : {}),
  }), bindings as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  expect((await call("get", { path: "MEMORY.md" }, false)).status).toBe(401);
  expect((await call("get", undefined, true, "GET")).status).toBe(405);
  expect((await call("get?scope=team", { path: "MEMORY.md" })).status).toBe(400);
  expect((await call("write", { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "private" })).status).toBe(403);
  expect(f.fetch).not.toHaveBeenCalled();
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(200);
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", record.organizationId, record.userId]));
  record.capabilities = ["memory:write"];
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(403);
  expect((await call("write", { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "private" })).status).toBe(200);
});

it("fetches fresh bootstrap every time but suppresses unchanged publication and observes deletions", async () => {
  const f = fixture();
  let documents = [{ path: "MEMORY.md", revision: 1, content: "saved fact" }];
  f.fetch.mockImplementation(async () => Response.json({ documents }));
  const session = { appendDeveloperMessage: vi.fn(async (_text: string) => {}) };
  const inject = () => injectMarkdownMemoryBootstrap(f.options, f.context, session, () => {});
  await inject();
  await inject();
  expect(f.fetch).toHaveBeenCalledTimes(4);
  expect(session.appendDeveloperMessage).toHaveBeenCalledTimes(1);
  expect(session.appendDeveloperMessage.mock.calls[0]![0]).toContain("USER.md");
  documents = [];
  await inject();
  expect(session.appendDeveloperMessage).toHaveBeenCalledTimes(2);
  expect(session.appendDeveloperMessage.mock.calls[1]![0]).toContain('"documents":[]');
  expect(session.appendDeveloperMessage.mock.calls[1]![0]).not.toContain("saved fact");
  const replacement = { appendDeveloperMessage: vi.fn(async (_text: string) => {}) };
  await injectMarkdownMemoryBootstrap(f.options, f.context, replacement, () => {});
  expect(replacement.appendDeveloperMessage).toHaveBeenCalledTimes(1);
});
it("bootstrap never loads personal memory for Connect and only marks successful appends as published", async () => {
  const f = fixture();
  f.connect();
  const session = { appendDeveloperMessage: vi.fn(async (_text: string) => {}) };
  session.appendDeveloperMessage.mockRejectedValueOnce(new Error("append failed"));
  await expect(injectMarkdownMemoryBootstrap(f.options, f.context, session, () => {})).rejects.toThrow("append failed");
  await injectMarkdownMemoryBootstrap(f.options, f.context, session, () => {});
  expect(session.appendDeveloperMessage).toHaveBeenCalledTimes(2);
  expect(f.fetch).toHaveBeenCalledTimes(2);
  expect(f.getByName.mock.calls.every(([name]) => name === "org")).toBe(true);
  expect(session.appendDeveloperMessage.mock.calls[1]![0]).not.toContain('"scope":"personal"');
});
it("withdraws stale bootstrap on a failed read and republishes the same snapshot after recovery", async () => {
  const f = fixture();
  f.connect();
  const session = { appendDeveloperMessage: vi.fn(async (_text: string) => {}) };
  const inject = () => injectMarkdownMemoryBootstrap(f.options, f.context, session, () => {});
  await inject();
  f.fetch.mockRejectedValueOnce(new Error("read unavailable"));
  await inject();
  expect(session.appendDeveloperMessage.mock.calls[1]![0]).toContain("Older snapshots may be stale");
  expect(session.appendDeveloperMessage.mock.calls[1]![0]).toContain("memory_get");
  await inject();
  expect(session.appendDeveloperMessage).toHaveBeenCalledTimes(3);
  expect(session.appendDeveloperMessage.mock.calls[2]![0]).toBe(session.appendDeveloperMessage.mock.calls[0]![0]);
});
