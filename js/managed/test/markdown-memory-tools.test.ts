import { env } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import worker from "../src/index";
import { MARKDOWN_BOOTSTRAP_TTL_MS, MarkdownMemoryBootstrapCache, injectMarkdownMemoryBootstrap, markdownMemoryEnabled, markdownMemoryRequest, markdownMemoryTools } from "../src/markdown-memory-tools";
import type { ManagedExtensionOptions } from "../src/extension-tools";
import { performanceScope } from "../src/performance";

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
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", "org", "alice"]), undefined);
  expect(f.fetch.mock.calls[0]).toEqual(["https://memory.internal/markdown-memory/get", expect.objectContaining({ headers: expect.objectContaining({ "x-nanocodex-private-memory-owner": "alice", "x-nanocodex-team-id": "personal:alice" }) })]);
  f.connect();
  await markdownMemoryRequest(f.options, "search", { query: "saved" }, f.context);
  expect(f.getByName).toHaveBeenLastCalledWith("org", undefined);
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
  expect(markdownMemoryTools(fixture().options).map(tool => tool.name)).toEqual(["memory_status", "memory_get", "memory_search", "memory_write"]);
  expect(markdownMemoryEnabled()).toBe(true);
  expect(markdownMemoryEnabled(["memory"])).toBe(true);
  expect(markdownMemoryEnabled(["memory_get"])).toBe(true);
  expect(markdownMemoryEnabled(["memory_write"])).toBe(false);
  expect(markdownMemoryEnabled(["memory_status"])).toBe(false);
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
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", record.organizationId, record.userId]), undefined);
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

it("requires live read authority for status and never exposes flush as a model tool", async () => {
  const f = fixture();
  const tools = markdownMemoryTools(f.options);
  expect(tools.some(tool => tool.name.includes("flush"))).toBe(false);
  const status = tools.find(tool => tool.name === "memory_status")!;
  f.fetch.mockResolvedValue(Response.json({ automation: "disabled", consolidation: { pending: [] }, flush: { receipts: [] } }));
  expect(await status.handler({}, f.context)).toMatchObject({ automation: "disabled", scope: "personal" });
  expect(f.authorize).toHaveBeenLastCalledWith("memory_get", f.context);
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
  const status = markdownMemoryTools(f.options).find(tool => tool.name === "memory_status")!;
  expect(await status.handler({}, f.context)).toMatchObject({ scope: "team" });
  expect(f.getByName).toHaveBeenLastCalledWith("org", undefined);
  expect(new Headers(f.fetch.mock.calls[0]![1]!.headers).has("x-nanocodex-private-memory-owner")).toBe(false);
  await expect(status.handler({ scope: "personal" }, f.context)).rejects.toThrow("direct account authority");
  expect(f.fetch).toHaveBeenCalledTimes(1);
});

function cachedFixture() {
  const f = fixture();
  f.fetch.mockImplementation(async () => Response.json({ documents: [{ path: "MEMORY.md", revision: 1, content: "saved fact" }] }));
  const cache = new MarkdownMemoryBootstrapCache();
  const session = { appendDeveloperMessage: vi.fn(async (_text: string) => {}) };
  let authority = "epoch:1/direct";
  const inject = () => injectMarkdownMemoryBootstrap(f.options, f.context, session, () => {}, { cache, authority });
  const snapshot = (scope: "personal" | "team" = "personal") => cache.snapshot(f.options, f.context, session, scope, authority, () => {});
  return { ...f, cache, session, inject, snapshot, authority: (value: string) => { authority = value; } };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

it("reuses startup excerpts only until the original read's expiry; explicit tools and replacements stay fresh", async () => {
  const clock = vi.spyOn(Date, "now").mockReturnValue(1000);
  try {
    const f = cachedFixture();
    await f.inject();
    clock.mockReturnValue(1000 + MARKDOWN_BOOTSTRAP_TTL_MS - 1);
    await f.inject();
    expect(f.fetch).toHaveBeenCalledTimes(2);
    expect(f.session.appendDeveloperMessage).toHaveBeenCalledTimes(1);
    const tools = markdownMemoryTools(f.options, f.cache);
    await tools.find(tool => tool.name === "memory_get")!.handler({ path: "MEMORY.md" }, f.context);
    await tools.find(tool => tool.name === "memory_search")!.handler({ query: "saved" }, f.context);
    expect(f.fetch).toHaveBeenCalledTimes(4);
    clock.mockReturnValue(1000 + MARKDOWN_BOOTSTRAP_TTL_MS);
    f.fetch.mockImplementation(async () => Response.json({ documents: [] }));
    await f.inject();
    expect(f.fetch).toHaveBeenCalledTimes(6);
    expect(f.session.appendDeveloperMessage.mock.calls[1]![0]).toContain('"documents":[]');
    const replacement = { appendDeveloperMessage: vi.fn(async () => {}) };
    await injectMarkdownMemoryBootstrap(f.options, f.context, replacement, () => {}, { cache: f.cache, authority: "epoch:1/direct" });
    expect(f.fetch).toHaveBeenCalledTimes(8);
    expect(replacement.appendDeveloperMessage).toHaveBeenCalledTimes(1);
  } finally { clock.mockRestore(); }
});

it.each([
  { operation: "put", path: "MEMORY.md", expected_revision: 0, content: "created" },
  { operation: "put", path: "MEMORY.md", expected_revision: 1, content: "updated" },
  { operation: "append", path: "memory/2026-09-23.md", expected_revision: 1, content: "appended", operation_id: "operation" },
  { operation: "delete", path: "MEMORY.md", expected_revision: 1 },
])("invalidates the relevant startup scope for explicit $operation at revision $expected_revision", async input => {
  const f = cachedFixture();
  await f.inject();
  await markdownMemoryTools(f.options, f.cache).find(tool => tool.name === "memory_write")!.handler(input, f.context);
  await f.inject();
  expect(f.fetch).toHaveBeenCalledTimes(4); // two initial scopes + write + personal refresh
  expect(f.fetch.mock.calls[3]![1]!.headers).toMatchObject({ "x-nanocodex-private-memory-owner": "alice" });
  await markdownMemoryTools(f.options, f.cache).find(tool => tool.name === "memory_write")!.handler({ ...input, scope: "team", user_requested: true }, f.context);
  await f.inject();
  expect(f.fetch).toHaveBeenCalledTimes(6);
  expect(new Headers(f.fetch.mock.calls[5]![1]!.headers).has("x-nanocodex-private-memory-owner")).toBe(false);
});

it("coalesces in-flight reads and retries failures without retaining an unavailable result", async () => {
  const f = cachedFixture();
  const pending = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pending.promise);
  const first = f.snapshot();
  const second = f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(1);
  pending.reject(new Error("unavailable"));
  expect((await Promise.allSettled([first, second])).every(result => result.status === "rejected")).toBe(true);
  await f.snapshot();
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(2);
});

it("coalesces successful reads without extending expiry by the read duration", async () => {
  const clock = vi.spyOn(Date, "now").mockReturnValue(1000);
  try {
    const f = cachedFixture();
    const pending = deferred<Response>();
    f.fetch.mockImplementationOnce(() => pending.promise);
    const first = f.snapshot();
    const second = f.snapshot();
    clock.mockReturnValue(1000 + MARKDOWN_BOOTSTRAP_TTL_MS - 1);
    pending.resolve(Response.json({ documents: [] }));
    expect(await Promise.all([first, second])).toEqual([{ documents: [], scope: "personal" }, { documents: [], scope: "personal" }]);
    expect(f.fetch).toHaveBeenCalledTimes(1);
    clock.mockReturnValue(1000 + MARKDOWN_BOOTSTRAP_TTL_MS);
    await f.snapshot();
    expect(f.fetch).toHaveBeenCalledTimes(2);
  } finally { clock.mockRestore(); }
});

it("rejects late bootstrap completions invalidated by writes, including uncertain writes", async () => {
  const f = cachedFixture();
  const pending = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pending.promise);
  const old = f.snapshot();
  f.fetch.mockRejectedValueOnce(new Error("write outcome unknown"));
  await expect(markdownMemoryRequest(f.options, "write", { operation: "delete", path: "MEMORY.md", expected_revision: 1 }, f.context, f.cache)).rejects.toThrow("unknown");
  pending.resolve(Response.json({ documents: [{ content: "deleted fact" }] }));
  await expect(old).rejects.toThrow("context changed");
  await f.snapshot();
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(3);
});

it("does not reuse or populate snapshots while a write is pending", async () => {
  const f = cachedFixture();
  await f.snapshot();
  const pending = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pending.promise);
  const write = markdownMemoryRequest(f.options, "write", { operation: "delete", path: "MEMORY.md", expected_revision: 1 }, f.context, f.cache);
  await expect(f.snapshot()).rejects.toThrow("context changed");
  pending.resolve(Response.json({ ok: true }));
  await write;
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(3);
});

it.each(["organizationId", "teamId", "ownerId", "sessionId"] as const)("isolates bootstrap by %s", async field => {
  const f = cachedFixture();
  await f.snapshot();
  f.options[field] = "different";
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(2);
});

it("isolates authority epochs and Connect scopes, rejecting old in-flight partitions", async () => {
  const f = cachedFixture();
  const pending = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pending.promise);
  const old = f.snapshot();
  f.authority("epoch:2/direct");
  await f.snapshot();
  pending.resolve(Response.json({ documents: [{ content: "old epoch" }] }));
  await expect(old).rejects.toThrow("context changed");
  f.connect();
  f.authority("epoch:2/connect-grant");
  await f.inject();
  expect(f.fetch).toHaveBeenCalledTimes(3);
  expect(f.session.appendDeveloperMessage.mock.calls[0]![0]).not.toContain('"scope":"personal"');
  await expect(f.snapshot()).rejects.toThrow("authority changed");
});

it("checks live authority and cancellation before serving a hit or publishing content", async () => {
  const f = cachedFixture();
  await f.inject();
  f.authorize.mockImplementation(() => { throw new Error("revoked"); });
  await f.inject();
  expect(f.fetch).toHaveBeenCalledTimes(2);
  expect(f.session.appendDeveloperMessage.mock.calls[1]![0]).toContain("Older snapshots may be stale");
  f.authorize.mockReset();
  const controller = new AbortController();
  controller.abort();
  await expect(f.cache.snapshot(f.options, { ...f.context, signal: controller.signal }, f.session, "personal", "epoch:1/direct", () => {})).rejects.toThrow();
  expect(f.fetch).toHaveBeenCalledTimes(2);
});

it("does not cache a cancelled in-flight load and retries durable append failures", async () => {
  const f = cachedFixture();
  const controller = new AbortController();
  const pending = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pending.promise);
  const cancelled = f.cache.snapshot(f.options, { ...f.context, signal: controller.signal }, f.session, "personal", "epoch:1/direct", () => {});
  const coalesced = f.snapshot();
  controller.abort();
  pending.resolve(Response.json({ documents: [] }));
  expect((await Promise.allSettled([cancelled, coalesced])).every(result => result.status === "rejected")).toBe(true);
  f.session.appendDeveloperMessage.mockRejectedValueOnce(new Error("append failed"));
  await expect(f.inject()).rejects.toThrow("append failed");
  await f.inject();
  expect(f.fetch).toHaveBeenCalledTimes(3);
  expect(f.session.appendDeveloperMessage).toHaveBeenCalledTimes(2);
});

it("does not retain oversized bootstrap payloads", async () => {
  const f = cachedFixture();
  f.fetch.mockImplementation(async () => Response.json({ documents: [{ content: "x".repeat(64 * 1024) }] }));
  await f.snapshot();
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(2);
});

it("records bounded bootstrap and append timings without contents or cache identities", async () => {
  const f = cachedFixture();
  const log = vi.spyOn(console, "info").mockImplementation(() => {});
  try {
    await performanceScope("trace", "turn.admission", f.inject);
    await performanceScope("trace", "turn.admission", f.inject);
    const records = log.mock.calls.map(call => call[0]);
    for (const stage of ["markdown.bootstrap.personal", "markdown.bootstrap.team", "markdown.bootstrap.append"])
      expect(records.some(record => record.stage === stage && record.success)).toBe(true);
    expect(records.filter(record => record.cache_state === "hit")).toHaveLength(2);
    const encoded = JSON.stringify(records);
    for (const privateValue of ["saved fact", "alice", "epoch:1/direct"]) expect(encoded).not.toContain(privateValue);
  } finally { log.mockRestore(); }
});

it("rejects in-flight identity changes even before another snapshot starts", async () => {
  const f = cachedFixture();
  const pending = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pending.promise);
  const old = f.snapshot();
  f.options.ownerId = "different";
  pending.resolve(Response.json({ documents: [{ content: "private old owner" }] }));
  await expect(old).rejects.toThrow("context changed");
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(2);
});

it("does not cache malformed or HTTP unavailable bootstrap responses", async () => {
  const f = cachedFixture();
  f.fetch.mockImplementationOnce(async () => Response.json({ error: "unavailable" }));
  await expect(f.snapshot()).rejects.toThrow("invalid memory bootstrap response");
  f.fetch.mockImplementationOnce(async () => Response.json({ error: "unavailable" }, { status: 503 }));
  await expect(f.snapshot()).rejects.toThrow("503");
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(3);
});

it("withdraws expired cached snapshots on read failure and republishes after recovery", async () => {
  const clock = vi.spyOn(Date, "now").mockReturnValue(1000);
  try {
    const f = cachedFixture();
    f.connect();
    await f.inject();
    clock.mockReturnValue(1000 + MARKDOWN_BOOTSTRAP_TTL_MS);
    f.fetch.mockRejectedValueOnce(new Error("read unavailable"));
    await f.inject();
    expect(f.session.appendDeveloperMessage.mock.calls[1]![0]).toContain("Older snapshots may be stale");
    await f.inject();
    expect(f.session.appendDeveloperMessage.mock.calls[2]![0]).toBe(f.session.appendDeveloperMessage.mock.calls[0]![0]);
    expect(f.fetch).toHaveBeenCalledTimes(3);
  } finally { clock.mockRestore(); }
});

it("rechecks already resolved scopes before publishing after another scope's slow read", async () => {
  const f = cachedFixture();
  await f.snapshot("personal");
  const pendingTeam = deferred<Response>();
  f.fetch.mockImplementationOnce(() => pendingTeam.promise);
  const inject = f.inject();
  await markdownMemoryRequest(f.options, "write", { operation: "delete", path: "MEMORY.md", expected_revision: 1 }, f.context, f.cache);
  pendingTeam.resolve(Response.json({ documents: [] }));
  await inject;
  expect(f.session.appendDeveloperMessage.mock.calls[0]![0]).toContain("Older snapshots may be stale");
  expect(f.session.appendDeveloperMessage.mock.calls[0]![0]).not.toContain('"content":"saved fact"');
  await f.inject();
  expect(f.fetch).toHaveBeenCalledTimes(4);
});

it.each([
  { documents: [], error: "unavailable" },
  { documents: [], status: "unavailable" },
  { documents: [], available: false },
  { documents: [], ok: false },
])("does not cache unavailable bootstrap envelopes even with documents: %j", async response => {
  const f = cachedFixture();
  f.fetch.mockImplementationOnce(async () => Response.json(response));
  await expect(f.snapshot()).rejects.toThrow("invalid memory bootstrap response");
  await f.snapshot();
  expect(f.fetch).toHaveBeenCalledTimes(2);
});

it("adds a first-use placement hint without changing the authorized memory partition", async () => {
  const f = fixture();
  f.options.clientIngressColo = "SJC";
  await markdownMemoryRequest(f.options, "get", { path: "MEMORY.md" }, f.context);
  expect(f.getByName).toHaveBeenLastCalledWith(JSON.stringify(["personal-memory", "org", "alice"]), { locationHint: "wnam" });
  f.connect();
  await markdownMemoryRequest(f.options, "get", { path: "MEMORY.md" }, f.context);
  expect(f.getByName).toHaveBeenLastCalledWith("org", { locationHint: "wnam" });
});
