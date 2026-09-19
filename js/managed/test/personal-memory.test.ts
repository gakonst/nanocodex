import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { MemoryScope } from "../src/memory-scope";
import worker, { type DurableAgentSession } from "../src/index";
import { memoryTarget, scopedMemoryOperation } from "../src/memory-target";
import { ManagedStartupContext } from "../src/startup-context";
import { PreparedPersonalizationCache, personalizationText, type PersonalizationSnapshot } from "../src/personalization";

const bindings = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope>; NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> };
function target(org: string, team: string, user: string, scope: "team" | "personal") {
  const address = memoryTarget(org, team, user, scope);
  return { stub: bindings.NANOCODEX_MEMORY.getByName(address.name), headers: {
    "x-nanocodex-organization-id": org, "x-nanocodex-team-id": address.team,
    "x-nanocodex-memory-initialize": "1", "x-nanocodex-subject-id": `user:${user}`,
    "x-nanocodex-memory-mutation": "1",
  } };
}
async function op(where: ReturnType<typeof target>, body: unknown) {
  return where.stub.fetch("https://memory.internal/memory", { method: "POST", headers: where.headers, body: JSON.stringify(body) });
}
it("isolates personal memories and scan receipts from teammates and other users, but retains them across teams", async () => {
  const org = crypto.randomUUID();
  const personal = target(org, "team-a", "alice", "personal");
  const team = target(org, "team-a", "alice", "team");
  expect((await op(team, { operation: "scan", query: "concise" })).status).toBe(200);
  expect((await op(personal, { operation: "put", content: "Alice prefers concise replies" })).status).toBe(400);
  expect((await op(personal, { operation: "scan", query: "concise" })).status).toBe(200);
  const put = await op(personal, { operation: "put", content: "Alice prefers concise replies" });
  expect(put.status).toBe(200);
  const saved = await put.json<{ memory: { key: { id: number; version: number } } }>();
  for (const other of [team, target(org, "team-a", "bob", "personal"), target(crypto.randomUUID(), "team-a", "alice", "personal")]) {
    const read = await op(other, { operation: "read", keys: [saved.memory.key] });
    expect(await read.json()).toMatchObject({ memories: [] });
  }
  const read = await op(target(org, "team-b", "alice", "personal"), { operation: "read", keys: [saved.memory.key] });
  expect(await read.json()).toMatchObject({ memories: [{ content: "Alice prefers concise replies" }] });
  expect(() => scopedMemoryOperation({ operation: "scan", query: "x", scope: "personal", user_id: "bob" })).toThrow();
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
  await op(where, { operation: "scan", query: "personal canary" });
  const put = await op(where, { operation: "put", content: "Alice personal canary" });
  const saved = await put.json<{ memory: { key: { id: number; version: number } } }>();
  const response = await where.stub.fetch("https://memory.internal/personalization", { method: "POST",
    headers: { ...where.headers, "x-nanocodex-personalization-user": "alice", "x-nanocodex-personalization-session": session.id.toString() } });
  const { snapshot } = await response.json<{ snapshot: PersonalizationSnapshot }>();
  const combined = { ...snapshot, team_id: "team", team_facts: [], user_facts: snapshot.team_facts, user_version: snapshot.version, user_generation: snapshot.generation };
  expect(personalizationText(combined)).toContain("Alice personal canary");
  await runInDurableObject(session, async (_obj, ctx) => { new ManagedStartupContext(ctx.storage).reservePrepared("pending", combined, false); });
  expect((await op(where, { operation: "delete", key: saved.memory.key })).status).toBe(200);
  await runInDurableObject(session, async (_obj, ctx) => {
    const startup = new ManagedStartupContext(ctx.storage);
    await startup.prepare("pending", vi.fn(), async () => undefined, () => {});
    expect(ctx.storage.sql.exec<{ content: string }>("SELECT content FROM managed_startup_context WHERE turn_id='pending'").one().content).not.toContain("personal canary");
    expect(ctx.storage.sql.exec<{ profile_json: string | null }>("SELECT profile_json FROM managed_prepared_personalization WHERE turn_id='pending'").one().profile_json).toBeNull();
  });
});
it("prevents an in-flight stale personal refresh from resurrecting a forgotten fact", async () => {
  const cache = new PreparedPersonalizationCache();
  const scope = { organization_id: "org", team_id: "team", user_id: "alice" };
  let resolve!: (snapshot: PersonalizationSnapshot) => void;
  const pending = new Promise<PersonalizationSnapshot>(r => { resolve = r; });
  const tasks: Promise<void>[] = [];
  cache.warm(scope, () => pending, task => tasks.push(task));
  cache.invalidate(2, "personal");
  resolve({ ...scope, generation: 100, version: "team:100", expires_at: Date.now() + 60_000, team_facts: [],
    user_generation: 1, user_version: "1:1", user_facts: [{ id: 1, version: 1, content: "forgotten" }] });
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
  expect((await request("POST", "?scope=personal", { operation: "scan", query: "private" })).status).toBe(400);
  expect((await request("POST", "", { operation: "scan", query: "private", scope: "personal" })).status).toBe(200);
  const put = await request("POST", "", { operation: "put", content: "Prefers private replies", scope: "personal" });
  expect(put.status).toBe(200);
  const saved = await put.json<{ memory: { key: { id: number; version: number } } }>();
  expect(await (await request("GET", "?scope=personal")).text()).toContain("Prefers private replies");
  expect(await (await request("GET", "")).text()).not.toContain("Prefers private replies");
  const canonical = (method: string, body: unknown) => worker.fetch(new Request(`https://test.example/v1/memories/${method}`, {
    method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(body),
  }), testEnv as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  const legacyPath = `legacy/${saved.memory.key.id}-v${saved.memory.key.version}.md`;
  expect(await (await canonical("read", { path: legacyPath })).json()).toMatchObject({ path: legacyPath, content: "Prefers private replies", start_line_number: 1, truncated: false });
  expect(await (await canonical("list", { max_results: 0 })).json()).toMatchObject({ entries: [{ path: "legacy", entry_type: "directory" }] });
  await request("POST", "", { operation: "scan", query: "shared team canary" });
  await request("POST", "", { operation: "put", content: "shared team canary" });
  const shared = await (await canonical("search", { queries: ["shared team canary"] })).json<{ matches: { path: string }[] }>();
  expect(shared.matches[0]?.path).toMatch(/^team\/legacy\//);
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
  expect(await (await request("GET", "?scope=personal")).text()).not.toContain("Prefers private replies");
  record.userId = alice;
  record.capabilities = ["memory:read"];
  expect((await request("DELETE", `/${saved.memory.key.id}?version=${saved.memory.key.version}&scope=personal`)).status).toBe(403);
  record.capabilities.push("memory:write");
  expect((await request("DELETE", `/${saved.memory.key.id}?version=${saved.memory.key.version}&scope=personal`)).status).toBe(204);
  expect(await (await request("GET", "?scope=personal")).text()).not.toContain("Prefers private replies");
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
