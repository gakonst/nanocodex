import { env } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import worker from "../src/index";
import { extensionSpecs } from "nanocodex-tools/extensions";
import { injectMarkdownMemoryBootstrap, markdownMemoryEnabled, markdownMemoryWriteEnabled, configuredMemoryToolNames, markdownMemoryRequest, markdownMemoryTools } from "../src/markdown-memory-tools";
import { managedExtensionTools, type ManagedExtensionOptions } from "../src/extension-tools";

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
  expect(f.authorize).toHaveBeenLastCalledWith("memories__write", f.context);
  expect(f.fetch.mock.calls[0]).toEqual([expect.any(String), expect.objectContaining({ headers: expect.objectContaining({ "x-nanocodex-memory-mutation": "1" }), body: JSON.stringify({ operation: "put", path: "MEMORY.md", expected_revision: 0, content: "shared" }) })]);
});
it("gates bootstrap by configured reads and exposes one memories namespace", () => {
  expect(markdownMemoryTools(fixture().options).map(tool => tool.name)).toEqual(["memories__status", "memories__get", "memories__search_markdown", "memories__write"]);
  expect(markdownMemoryEnabled()).toBe(true);
  expect(markdownMemoryEnabled(["memory"])).toBe(true);
  expect(markdownMemoryEnabled(["memories__get"])).toBe(true);
  expect(markdownMemoryEnabled(["memories__write"])).toBe(false);
  expect(markdownMemoryEnabled(["memories__status"])).toBe(false);
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
  expect((await call("status", {}, false)).status).toBe(401);
  expect((await call("status", undefined, true, "GET")).status).toBe(405);
  expect((await call("status?scope=team", {})).status).toBe(400);
  expect((await call("flush", { session_id: "session", boundary_id: "boundary", messages: [] })).status).toBe(404);
  expect((await call("unsupported", {})).status).toBe(404);
  expect((await call("get", undefined, true, "GET")).status).toBe(405);
  expect((await call("get?scope=team", { path: "MEMORY.md" })).status).toBe(400);
  expect((await call("write", { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "private" })).status).toBe(403);
  expect(f.fetch).not.toHaveBeenCalled();
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(200);
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", record.organizationId, record.userId]));
  expect((await call("status", {})).status).toBe(200);
  expect(f.fetch).toHaveBeenLastCalledWith("https://memory.internal/markdown-memory/status", expect.objectContaining({
    headers: expect.objectContaining({ "x-nanocodex-private-memory-owner": record.userId }), body: "{}",
  }));
  expect((await call("status", { scope: "team" })).status).toBe(200);
  expect(f.getByName).toHaveBeenLastCalledWith(record.organizationId);
  record.capabilities = ["memory:write"];
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(403);
  const requestsBeforeDeniedStatus = f.fetch.mock.calls.length;
  expect((await call("status", {})).status).toBe(403);
  expect(f.fetch).toHaveBeenCalledTimes(requestsBeforeDeniedStatus);
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
  expect(session.appendDeveloperMessage.mock.calls[1]![0]).toContain("memories__get");
  await inject();
  expect(session.appendDeveloperMessage).toHaveBeenCalledTimes(3);
  expect(session.appendDeveloperMessage.mock.calls[2]![0]).toBe(session.appendDeveloperMessage.mock.calls[0]![0]);
});

it("requires live read authority for status and never exposes flush as a model tool", async () => {
  const f = fixture();
  const tools = markdownMemoryTools(f.options);
  expect(tools.some(tool => tool.name.includes("flush"))).toBe(false);
  const status = tools.find(tool => tool.name === "memories__status")!;
  f.fetch.mockResolvedValue(Response.json({ automation: "disabled", consolidation: { pending: [] }, flush: { receipts: [] } }));
  expect(await status.handler({}, f.context)).toMatchObject({ automation: "disabled", scope: "personal" });
  expect(f.authorize).toHaveBeenLastCalledWith("memories__status", f.context);
  expect(f.fetch).toHaveBeenLastCalledWith("https://memory.internal/markdown-memory/status", expect.objectContaining({
    headers: expect.objectContaining({ "x-nanocodex-private-memory-owner": "alice", "x-nanocodex-subject-id": "agent:session" }),
  }));
  expect(new Headers(f.fetch.mock.calls[0]![1]!.headers).has("x-nanocodex-memory-mutation")).toBe(false);
  f.authorize.mockImplementation(() => { throw new Error("memory:read capability is required"); });
  await expect(status.handler({}, f.context)).rejects.toThrow("memory:read capability is required");
  expect(f.fetch).toHaveBeenCalledTimes(1);
});

it("keeps Connect status in its authorized team and blocks private status reads", async () => {
  const f = fixture();
  f.connect();
  const status = markdownMemoryTools(f.options).find(tool => tool.name === "memories__status")!;
  expect(await status.handler({}, f.context)).toMatchObject({ scope: "team" });
  expect(f.getByName).toHaveBeenLastCalledWith("org");
  expect(new Headers(f.fetch.mock.calls[0]![1]!.headers).has("x-nanocodex-private-memory-owner")).toBe(false);
  await expect(status.handler({ scope: "personal" }, f.context)).rejects.toThrow("direct account authority");
  expect(f.fetch).toHaveBeenCalledTimes(1);
});


it("keeps the four Codex declarations unchanged and every memory tool in one namespace", () => {
  const options = fixture().options;
  const pinned = managedExtensionTools(options);
  expect(pinned.map(({ handler: _handler, ...spec }) => spec)).toEqual(extensionSpecs);
  const all = [...pinned, ...markdownMemoryTools(options)];
  expect(new Set(all.map(tool => tool.name)).size).toBe(8);
  expect(all.every(tool => tool.name.startsWith("memories__"))).toBe(true);
  for (const tool of markdownMemoryTools(options)) {
    const schema = JSON.stringify(tool.parameters);
    expect(schema).not.toContain("expected_revision");
    expect(schema).not.toContain("operation_id");
    expect(schema).not.toContain('"revision"');
  }
});
it("normalizes legacy configuration without registering duplicate tools", () => {
  expect(configuredMemoryToolNames()).toBeUndefined();
  expect(configuredMemoryToolNames([])).toEqual([]);
  expect(configuredMemoryToolNames(["memory_get", "memories__get", "memory_search", "memory_write", "memory_status"]))
    .toEqual(["memories__get", "memories__search_markdown", "memories__write", "memories__status"]);
  expect(configuredMemoryToolNames(["memory"])).toHaveLength(8);
  for (const name of ["memory_get", "memory_search", "memories__get", "memories__search_markdown", "memories__read", "memories__search"])
    expect(markdownMemoryEnabled([name])).toBe(true);
  for (const names of [undefined, ["memory"], ["memory_write"], ["memories__write"]]) expect(markdownMemoryWriteEnabled(names)).toBe(true);
  expect(markdownMemoryWriteEnabled([])).toBe(false);
  expect(markdownMemoryWriteEnabled(["memories__add_ad_hoc_note"])).toBe(false);
});
it("accepts plain writes and owns delivery identity without exposing storage bookkeeping", async () => {
  const f = fixture();
  const write = markdownMemoryTools(f.options).find(tool => tool.name === "memories__write")!;
  f.fetch.mockImplementation(async () => Response.json({ ok: true, path: "MEMORY.md", revision: 3, replayed: true, deleted: false }));
  const input = { operation: "put", path: "MEMORY.md", content: "Prefer concise answers." };
  expect(await write.handler(input, f.context)).toEqual({ ok: true, path: "MEMORY.md", deleted: false, scope: "personal" });
  await write.handler(input, f.context);
  await write.handler(input, { ...f.context, callId: "another-call" });
  const bodies = f.fetch.mock.calls.map(([, init]) => JSON.parse(init!.body as string));
  expect(bodies[0]).toEqual({ ...input, operation_id: expect.stringMatching(/^[a-f0-9]{64}$/) });
  expect(bodies[1]).toEqual(bodies[0]);
  expect(bodies[2].operation_id).not.toBe(bodies[0].operation_id);
  expect(f.authorize).toHaveBeenLastCalledWith("memories__write", expect.objectContaining({ callId: "another-call" }));
});
it("exposes simplified get/write/status through the canonical memories HTTP routes", async () => {
  const token = `ncx_live_${"n".repeat(12)}_${"t".repeat(43)}`;
  const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token))))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = { id: "n".repeat(12), prefix: `ncx_live_${"n".repeat(12)}`, digest, label: "fixture", createdAt: 1,
    userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "writer", authorizationEpoch: 1, capabilities: ["memory:read", "memory:write"] };
  const f = fixture();
  const bindings = { ...env, NANOCODEX_MEMORY: f.options.memories, NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: async () => record }) } };
  const call = (method: string, body: unknown) => worker.fetch(new Request(`https://test.example/v1/memories/${method}`, {
    method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(body),
  }), bindings as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(200);
  expect((await call("search_markdown", { query: "saved" })).status).toBe(200);
  expect((await call("status", {})).status).toBe(200);
  const input = { operation: "put", path: "MEMORY.md", content: "saved" };
  expect((await call("write", input)).status).toBe(200);
  expect((await call("write", input)).status).toBe(200);
  const bodies = f.fetch.mock.calls.slice(-2).map(([, init]) => JSON.parse(init!.body as string));
  expect(bodies[0].operation_id).not.toBe(bodies[1].operation_id);
  expect(bodies[0].expected_revision).toBeUndefined();
  record.capabilities = ["memory:read"];
  expect((await call("write", input)).status).toBe(403);
});

it("runs simple namespaced writes and replays against real Durable Object storage", async () => {
  const f = fixture();
  f.options.organizationId = crypto.randomUUID();
  f.options.ownerId = crypto.randomUUID();
  f.options.sessionId = crypto.randomUUID();
  f.options.memories = (env as unknown as { NANOCODEX_MEMORY: ManagedExtensionOptions["memories"] }).NANOCODEX_MEMORY;
  const tools = markdownMemoryTools(f.options);
  const call = (name: string, input: unknown, callId: string) => tools.find(tool => tool.name === `memories__${name}`)!
    .handler(input, { ...f.context, sessionId: f.options.sessionId, callId });
  const note = { operation: "put", path: "MEMORY.md", content: "Prefer short status updates." };
  expect(await call("write", note, "put-1")).toMatchObject({ ok: true, path: note.path });
  const read = await call("get", { path: note.path }, "read-1");
  expect(read).toMatchObject({ content: note.content });
  expect(read).not.toHaveProperty("revision");
  const daily = { operation: "append", path: "memory/2026-09-22.md", content: "Finished the fixture task." };
  await call("write", daily, "append-1");
  await call("write", daily, "append-1");
  expect(await call("get", { path: daily.path }, "read-2")).toMatchObject({ content: daily.content });
  await call("write", { operation: "delete", path: note.path }, "delete-1");
  await call("write", note, "put-1");
  expect(await call("get", { path: note.path }, "read-3")).toMatchObject({ deleted: true, content: "" });
});
