import { env } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import worker from "../src/index";
import { preparedMarkdownText, markdownMemoryRequest, markdownMemoryTools } from "../src/markdown-memory-tools";
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
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", record.organizationId, record.userId]), undefined);
  expect((await call("search", { query: "saved" })).status).toBe(200);
  expect(f.fetch).toHaveBeenLastCalledWith("https://memory.internal/markdown-memory/search", expect.objectContaining({ body: JSON.stringify({ query: "saved" }) }));
  expect((await call("status", {})).status).toBe(200);
  expect(f.fetch).toHaveBeenLastCalledWith("https://memory.internal/markdown-memory/status", expect.objectContaining({
    headers: expect.objectContaining({ "x-nanocodex-private-memory-owner": record.userId }), body: "{}",
  }));
  expect((await call("status", { scope: "team" })).status).toBe(200);
  expect(f.getByName).toHaveBeenLastCalledWith(record.organizationId, undefined);
  record.capabilities = ["memory:write"];
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(403);
  const requestsBeforeDeniedStatus = f.fetch.mock.calls.length;
  expect((await call("status", {})).status).toBe(403);
  expect(f.fetch).toHaveBeenCalledTimes(requestsBeforeDeniedStatus);
  expect((await call("write", { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "private" })).status).toBe(200);
});

it("renders prepared Markdown without I/O and preserves bounded scoped excerpts", () => {
  const document = { path: "USER.md", revision: 1, content: '"\\<🦊'.repeat(5000), truncated: false };
  const text = preparedMarkdownText({ team_markdown: { documents: [document] }, user_markdown: { documents: [document] } })!;
  expect(new TextEncoder().encode(text).byteLength).toBeLessThan(32000);
  expect(text).not.toContain("<");
  expect(text).not.toContain("\ufffd");
  const scopes = JSON.parse(text.slice(text.indexOf("\n") + 1));
  expect(scopes.map((snapshot: { scope: string }) => snapshot.scope)).toEqual(["personal", "team"]);
  expect(scopes.every((snapshot: { truncated: boolean }) => snapshot.truncated)).toBe(true);
  expect(preparedMarkdownText()).toBeUndefined();
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
  expect(f.getByName).toHaveBeenLastCalledWith("org", undefined);
  expect(new Headers(f.fetch.mock.calls[0]![1]!.headers).has("x-nanocodex-private-memory-owner")).toBe(false);
  await expect(status.handler({ scope: "personal" }, f.context)).rejects.toThrow("direct account authority");
  expect(f.fetch).toHaveBeenCalledTimes(1);
});
it("exposes six canonical HTTP routes with Codex read/search contracts and rejects duplicate routes", async () => {
  const token = `ncx_live_${"n".repeat(12)}_${"t".repeat(43)}`;
  const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token))))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = { id: "n".repeat(12), prefix: `ncx_live_${"n".repeat(12)}`, digest, label: "fixture", createdAt: 1,
    userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "writer", authorizationEpoch: 1, capabilities: ["memory:read", "memory:write"] };
  const bindings = { ...env, NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: async () => record }) } };
  const call = (method: string, body: unknown) => worker.fetch(new Request(`https://test.example/v1/memories/${method}`, {
    method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(body),
  }), bindings as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  expect((await call("get", { path: "MEMORY.md" })).status).toBe(404);
  expect((await call("search_markdown", { query: "saved" })).status).toBe(404);
  expect((await call("status", {})).status).toBe(200);
  const input = { operation: "put", path: "MEMORY.md", content: "saved fixture\nsecond line" };
  const written = await call("write", input);
  expect(written.status).toBe(200);
  expect(await written.json()).toEqual({ ok: true, path: input.path, deleted: false, scope: "personal" });
  const read = await call("read", { path: input.path, line_offset: 2, max_lines: 1 });
  expect(read.status).toBe(200);
  expect(await read.json()).toEqual({ path: input.path, content: "second line", start_line_number: 2, truncated: false });
  const search = await call("search", { path: input.path, queries: ["saved"] });
  expect(search.status).toBe(200);
  expect(await search.json()).toEqual({ queries: ["saved"], match_mode: { type: "any" }, path: input.path,
    matches: [{ path: input.path, match_line_number: 1, content_start_line_number: 1, content: "saved fixture", matched_queries: ["saved"] }],
    next_cursor: null, truncated: false });
  expect((await call("list", {})).status).toBe(200);
  expect((await call("add_ad_hoc_note", { filename: "2026-09-22T10-00-00-fixture.md", note: "explicit fixture note" })).status).toBe(200);
  expect((await call("read", { path: input.path, from_line: 2 })).status).toBe(400);
  expect((await call("search", { query: "saved" })).status).toBe(400);
  record.capabilities = ["memory:read"];
  expect((await call("write", input)).status).toBe(403);
});

it("reads and searches canonical Markdown through the exact Codex contracts on real storage", async () => {
  const f = fixture();
  f.options.organizationId = crypto.randomUUID();
  f.options.ownerId = crypto.randomUUID();
  f.options.sessionId = crypto.randomUUID();
  f.options.memories = (env as unknown as { NANOCODEX_MEMORY: ManagedExtensionOptions["memories"] }).NANOCODEX_MEMORY;
  const tools = [...managedExtensionTools(f.options), ...markdownMemoryTools(f.options)];
  const call = (name: string, input: unknown, callId: string) => tools.find(tool => tool.name === `memories__${name}`)!
    .handler(input, { ...f.context, sessionId: f.options.sessionId, callId });
  const notes = [
    { path: "MEMORY.md", content: "Facts\nfixture decision\nnext fact" },
    { path: "USER.md", content: "Preferences\nfixture preference\nnext preference" },
    { path: "memory/2026-09-22.md", content: "Progress\nfixture task\nnext task" },
  ];
  for (const note of notes) {
    expect(await call("write", { operation: "put", ...note }, `put-${note.path}`)).toEqual({ ok: true, path: note.path, deleted: false, scope: "personal" });
    expect(await call("read", { path: note.path }, `read-${note.path}`)).toEqual({ path: note.path, content: note.content, start_line_number: 1, truncated: false });
    expect(await call("read", { path: note.path, line_offset: 2, max_lines: 1 }, `line-${note.path}`)).toEqual({
      path: note.path, content: note.content.split("\n")[1] + "\n", start_line_number: 2, truncated: true,
    });
  }
  await call("write", { operation: "put", path: "DREAMS.md", content: "fixture audit" }, "audit");
  expect(await call("search", { queries: ["fixture"], context_lines: 0, max_results: 10 }, "search")).toEqual({
    queries: ["fixture"], match_mode: { type: "any" }, path: null,
    matches: notes.map(note => ({ path: note.path, match_line_number: 2, content_start_line_number: 2,
      content: note.content.split("\n")[1], matched_queries: ["fixture"] })), next_cursor: null, truncated: false,
  });
  await expect(call("read", { path: "MEMORY.md", from_line: 2 }, "bad-read")).rejects.toThrow("unknown field from_line");
  await expect(call("read", { path: "MEMORY.md", scope: "personal" }, "bad-scope")).rejects.toThrow("unknown field scope");
  await expect(call("search", { query: "fixture" }, "bad-search")).rejects.toThrow("missing queries");
  const daily = { operation: "append", path: "memory/2026-09-23.md", content: "Finished the task." };
  await call("write", daily, "append-1");
  await call("write", daily, "append-1");
  expect(await call("read", { path: daily.path }, "read-append")).toEqual({ path: daily.path, content: daily.content, start_line_number: 1, truncated: false });
  await call("write", { operation: "delete", path: notes[0]!.path }, "delete-1");
  await call("write", { operation: "put", ...notes[0]! }, `put-${notes[0]!.path}`);
  await expect(call("read", { path: notes[0]!.path }, "read-deleted")).rejects.toThrow("memory file was not found");
  f.authorize.mockImplementation(() => { throw new Error("revoked"); });
  await expect(call("read", { path: "USER.md" }, "revoked-read")).rejects.toThrow("revoked");
  await expect(call("search", { queries: ["fixture"] }, "revoked-search")).rejects.toThrow("revoked");
});

it("uses team paths for shared canonical reads and keeps Connect in its team partition", async () => {
  const f = fixture();
  f.options.organizationId = crypto.randomUUID();
  f.options.ownerId = crypto.randomUUID();
  f.options.sessionId = crypto.randomUUID();
  f.options.memories = (env as unknown as { NANOCODEX_MEMORY: ManagedExtensionOptions["memories"] }).NANOCODEX_MEMORY;
  const tools = [...managedExtensionTools(f.options), ...markdownMemoryTools(f.options)];
  const call = (name: string, input: unknown, callId: string) => tools.find(tool => tool.name === `memories__${name}`)!
    .handler(input, { ...f.context, sessionId: f.options.sessionId, callId });
  await call("write", { operation: "put", path: "USER.md", content: "private fixture" }, "private");
  await call("write", { operation: "put", path: "MEMORY.md", content: "shared fixture", scope: "team", user_requested: true }, "shared");
  expect(await call("read", { path: "team/MEMORY.md" }, "shared-read")).toEqual({ path: "team/MEMORY.md", content: "shared fixture", start_line_number: 1, truncated: false });
  expect(await call("search", { path: "team/", queries: ["fixture"] }, "shared-search")).toEqual({
    queries: ["fixture"], match_mode: { type: "any" }, path: "team/", next_cursor: null, truncated: false,
    matches: [{ path: "team/MEMORY.md", content: "shared fixture", match_line_number: 1, content_start_line_number: 1, matched_queries: ["fixture"] }],
  });
  f.connect();
  expect(await call("read", { path: "MEMORY.md" }, "connect-read")).toEqual({ path: "MEMORY.md", content: "shared fixture", start_line_number: 1, truncated: false });
  expect(await call("search", { queries: ["fixture"] }, "connect-search")).toEqual({
    queries: ["fixture"], match_mode: { type: "any" }, path: null, next_cursor: null, truncated: false,
    matches: [{ path: "MEMORY.md", content: "shared fixture", match_line_number: 1, content_start_line_number: 1, matched_queries: ["fixture"] }],
  });
  await expect(call("read", { path: "USER.md" }, "private-read")).rejects.toThrow("memory file was not found");
});
