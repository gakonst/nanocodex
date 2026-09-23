import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { MemoryScope } from "../src/memory-scope";
import worker, { type DurableAgentSession } from "../src/index";
import { memoryTarget } from "../src/memory-target";
import { ManagedStartupContext } from "../src/startup-context";
import { preparedMarkdownText } from "../src/markdown-memory-tools";
import { PreparedPersonalizationCache, type PersonalizationSnapshot } from "../src/personalization";

const bindings = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope>; NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> };
function target(org: string, team: string, user: string, scope: "team" | "personal") {
  const address = memoryTarget(org, team, user, scope);
  return { stub: bindings.NANOCODEX_MEMORY.getByName(address.name), headers: {
    "x-nanocodex-organization-id": org, "x-nanocodex-team-id": address.team,
    "x-nanocodex-memory-initialize": "1", "x-nanocodex-subject-id": `user:${user}`,
    "x-nanocodex-memory-mutation": "1",
    ...(scope === "personal" ? { "x-nanocodex-private-memory-owner": user } : {}),
  } };
}
async function op(where: ReturnType<typeof target>, method: string, body: unknown) {
  const path = method === "write" ? "markdown-memory/write" : `extension-memories/${method}`;
  return where.stub.fetch(`https://memory.internal/${path}`, { method: "POST", headers: where.headers, body: JSON.stringify(body) });
}
it("isolates personal Markdown from teammates and other users, but retains it across teams", async () => {
  const org = crypto.randomUUID();
  const personal = target(org, "team-a", "alice", "personal");
  const team = target(org, "team-a", "alice", "team");
  const note = { path: "USER.md", content: "Alice prefers concise replies" };
  expect((await op(personal, "write", { operation: "put", ...note })).status).toBe(200);
  for (const other of [team, target(org, "team-a", "bob", "personal"), target(crypto.randomUUID(), "team-a", "alice", "personal")]) {
    expect(await (await op(other, "search", { queries: ["concise"] })).json()).toMatchObject({ matches: [] });
    expect((await op(other, "read", { path: note.path })).status).toBe(400);
  }
  const read = await op(target(org, "team-b", "alice", "personal"), "read", { path: note.path });
  expect(await read.json()).toEqual({ ...note, start_line_number: 1, truncated: false });
  expect((await op(personal, "read", { path: note.path, user_id: "bob" })).status).toBe(400);
  const wrongOwner = { ...personal, headers: { ...personal.headers, "x-nanocodex-private-memory-owner": "bob" } };
  expect((await op(wrongOwner, "read", { path: note.path })).status).toBe(403);
});
it("fences pending combined startup snapshots when a personal memory is forgotten", async () => {
  const org = crypto.randomUUID();
  const where = target(org, "team", "alice", "personal");
  const session = bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID());
  await runInDurableObject(session, async (_obj, ctx) => {
    ctx.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
      authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active)
      VALUES(1,?,'alice',?,'team',1,'https://test.example','managed',0,?)`, crypto.randomUUID(), org, Date.now());
  });
  expect((await op(where, "write", { operation: "put", path: "USER.md", content: "Alice personal canary" })).status).toBe(200);
  const response = await where.stub.fetch("https://memory.internal/personalization", { method: "POST",
    headers: { ...where.headers, "x-nanocodex-personalization-user": "alice", "x-nanocodex-personalization-session": session.id.toString() } });
  const { snapshot } = await response.json<{ snapshot: PersonalizationSnapshot }>();
  const combined = { ...snapshot, team_id: "team", team_markdown: { documents: [] }, user_markdown: snapshot.team_markdown, user_version: snapshot.version, user_generation: snapshot.generation };
  expect(preparedMarkdownText(combined)).toContain("Alice personal canary");
  await runInDurableObject(session, async (_obj, ctx) => { new ManagedStartupContext(ctx.storage).reservePrepared("pending", combined, false); });
  expect((await op(where, "write", { operation: "delete", path: "USER.md" })).status).toBe(200);
  await runInDurableObject(where.stub, async memory => { await memory.alarm(); });
  await runInDurableObject(session, async (_obj, ctx) => {
    const startup = new ManagedStartupContext(ctx.storage);
    await startup.prepare("pending", vi.fn(), async () => undefined);
    expect(ctx.storage.sql.exec<{ content: string }>("SELECT content FROM managed_startup_context WHERE turn_id='pending'").one().content).not.toContain("personal canary");
    expect(ctx.storage.sql.exec<{ profile_json: string | null }>("SELECT profile_json FROM managed_prepared_personalization WHERE turn_id='pending'").one().profile_json).toBeNull();
  });
});
it("prevents an in-flight stale personal refresh from resurrecting a forgotten note", async () => {
  const cache = new PreparedPersonalizationCache();
  const scope = { organization_id: "org", team_id: "team", user_id: "alice" };
  let resolve!: (snapshot: PersonalizationSnapshot) => void;
  const pending = new Promise<PersonalizationSnapshot>(r => { resolve = r; });
  const tasks: Promise<void>[] = [];
  cache.warm(scope, () => pending, task => tasks.push(task));
  cache.invalidate(2, "personal");
  resolve({ ...scope, generation: 100, version: "team:100", expires_at: Date.now() + 60_000, team_markdown: { documents: [] },
    user_generation: 1, user_version: "1:1", user_markdown: { documents: [{ path: "USER.md", revision: 1, content: "forgotten", truncated: false }] } });
  await Promise.all(tasks);
  expect(cache.peek(scope)).toBeUndefined();
});

it("routes public personal-memory operations by authenticated identity and rejects ambiguous scope", async () => {
  const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
  const digest = btoa(String.fromCharCode(...new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)),
  ))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = {
    id: "k".repeat(12), prefix: `ncx_live_${"k".repeat(12)}`, digest,
    label: "memory", createdAt: 1, userId: crypto.randomUUID(),
    organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "writer",
    authorizationEpoch: 1, capabilities: ["memory:read", "memory:write"],
  };
  const testEnv = { ...env, NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: async () => record }) } };
  const request = (method: string, path: string, body?: unknown) => worker.fetch(new Request(`https://test.example/v1/memory${path}`, {
    method, headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }), testEnv as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  expect((await request("POST", "", { operation: "scan", query: "private", scope: "personal" })).status).toBe(404);
  expect((await request("POST", "", { operation: "put", content: "retired" })).status).toBe(404);
  expect((await request("GET", "?scope=personal")).status).toBe(404);
  expect((await request("DELETE", "/1?version=1&scope=personal")).status).toBe(404);
  const canonical = (method: string, body: unknown) => worker.fetch(new Request(`https://test.example/v1/memories/${method}`, {
    method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(body),
  }), testEnv as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  expect((await canonical("write", { operation: "put", path: "USER.md", content: "Prefers private replies" })).status).toBe(200);
  expect(await (await canonical("read", { path: "USER.md" })).json()).toEqual({ path: "USER.md", content: "Prefers private replies", start_line_number: 1, truncated: false });
  expect(await (await canonical("list", {})).json()).toMatchObject({ entries: [{ path: "USER.md", entry_type: "file" }] });
  expect((await canonical("read", { path: "USER.md", scope: "personal" })).status).toBe(400);
  expect((await canonical("read", { path: "legacy/1-v1.md" })).status).toBe(400);
  const sharedNote = { operation: "put", path: "MEMORY.md", content: "shared team canary", scope: "team" };
  expect((await canonical("write", sharedNote)).status).toBe(403);
  expect((await canonical("write", { ...sharedNote, user_requested: true })).status).toBe(200);
  const shared = await (await canonical("search", { queries: ["shared team canary"] })).json<{ matches: { path: string }[] }>();
  expect(shared.matches.map(match => match.path)).toEqual(["team/MEMORY.md"]);
  const note = { filename: "2026-09-19T10-30-00-private-test.md", note: "new private canary\n" };
  expect(await (await canonical("add_ad_hoc_note", note)).json()).toEqual({});
  expect((await canonical("add_ad_hoc_note", note)).status).toBe(400);
  expect(await (await canonical("read", { path: `extensions/ad_hoc/notes/${note.filename}` })).json()).toMatchObject({ content: note.note });
  expect((await canonical("read", { path: "../other-user" })).status).toBe(400);
  expect((await canonical("read", { path: 123 })).status).toBe(400);
  record.capabilities = ["memory:read"];
  expect((await canonical("add_ad_hoc_note", { ...note, filename: "2026-09-19T10-30-00-denied.md" })).status).toBe(403);
  record.capabilities.push("memory:write");
  const alice = record.userId;
  record.userId = crypto.randomUUID();
  expect(await (await canonical("search", { queries: ["private canary"] })).json()).toMatchObject({ matches: [] });
  expect((await canonical("read", { path: "USER.md" })).status).toBe(400);
  record.userId = alice;
  record.capabilities = ["memory:read"];
  expect((await canonical("write", { operation: "delete", path: "USER.md" })).status).toBe(403);
  record.capabilities.push("memory:write");
  expect((await canonical("write", { operation: "delete", path: "USER.md" })).status).toBe(200);
  expect((await canonical("read", { path: "USER.md" })).status).toBe(400);
});

it("keeps Connect memory roots team-only and rechecks authority on every call", async () => {
  const { managedExtensionTools } = await import("../src/extension-tools");
  const organizationId = crypto.randomUUID(), teamId = crypto.randomUUID(), ownerId = crypto.randomUUID();
  let personal = true, allowed = true;
  const tools = managedExtensionTools({ organizationId, teamId, ownerId, sessionId: crypto.randomUUID(),
    memories: bindings.NANOCODEX_MEMORY, personal: () => personal,
    authorize: () => { if (!allowed) throw new Error("forbidden"); },
  });
  const context = { sessionId: "test", callId: "test", parentCallId: "", model: "test", signal: new AbortController().signal };
  const call = (name: string, input: unknown) => tools.find(tool => tool.name === `memories__${name}`)!.handler(input, context);
  const note = { filename: "2026-09-19T10-30-00-private.md", note: "personal canary" };
  await call("add_ad_hoc_note", note);
  personal = false;
  expect(await call("search", { queries: ["personal canary"] })).toMatchObject({ matches: [] });
  await expect(call("read", { path: `team/extensions/ad_hoc/notes/${note.filename}` })).rejects.toThrow();
  await call("add_ad_hoc_note", { ...note, filename: "2026-09-19T10-30-00-team.md", note: "team canary" });
  personal = true;
  expect(await call("read", { path: "team/extensions/ad_hoc/notes/2026-09-19T10-30-00-team.md" })).toMatchObject({ content: "team canary" });
  allowed = false;
  await expect(call("read", { path: `extensions/ad_hoc/notes/${note.filename}` })).rejects.toThrow("forbidden");
  await expect(call("add_ad_hoc_note", { ...note, filename: "2026-09-19T10-30-00-denied.md" })).rejects.toThrow("forbidden");
});
