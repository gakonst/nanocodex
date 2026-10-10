import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";

import {
  adoptImportedSubagentRoute,
  applyManagedSubagentLifecycle,
  managedAuthorizationForRouting,
  managedAuthorizationForToolContext,
  managedImportedSubagentAuthority,
  ManagedSubagentBindings,
  type DurableAgentSession,
} from "../src/index";

// A managed import restores the source task tree; the spawning turn named by a
// root-level child's journal host context belongs to the source agent and is
// never a destination managed_turns row.
const ROOT = "01996666-6666-7666-8666-666666666666";
const OTHER_ROOT = "01997777-7777-7777-8777-777777777777";
const CHILD = "01998888-8888-7888-8888-888888888888";
const GRANDCHILD = "01999999-9999-7999-8999-999999999999";
const SIBLING = "0199aaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa";
const SOURCE_TURN = "source-spawning-turn";
const account = { capabilities: ["agents:write", "tools:use"] as const };
const reduced = {
  capabilities: ["agents:write", "tools:use"] as const,
  connectGrant: { grantId: `0x${"b".repeat(64)}`, connectors: ["chatgpt"] as const, mcpIds: [] as const },
};

describe("managed import child authority", () => {
  it("falls back only on an adopted agent, for its own root, with a live turn", async () => {
    await withSession(async (storage) => {
      seedRoot(storage);
      expect(managedImportedSubagentAuthority(storage, ROOT, account)).toBeUndefined();
      adopt(storage);
      expect(managedImportedSubagentAuthority(storage, ROOT, undefined)).toBeUndefined();
      expect(managedImportedSubagentAuthority(storage, OTHER_ROOT, account)).toBeUndefined();
      expect(managedImportedSubagentAuthority(storage, ROOT, reduced)).toBe(reduced);
    });
  });

  it("binds an imported child with exactly the live destination turn's narrower authority", async () => {
    await withSession(async (storage, bindings) => {
      const child = descriptor("1", null, CHILD);
      expect(() => bind(storage, bindings, child, SOURCE_TURN, () => undefined))
        .toThrow("managed subagent authorization turn is missing");
      expect(() => bind(storage, bindings, child, SOURCE_TURN)).toThrow("managed subagent authorization turn is missing");
      expect(bindings.authorizations.size).toBe(0);
      bind(storage, bindings, child, SOURCE_TURN, () => reduced);
      expect(bindings.authorizations.get(CHILD)?.host_context_ref).toBe(SOURCE_TURN);
      // A broader later root turn never widens the child.
      expect(managedAuthorizationForToolContext(bindings, ROOT, account, { sessionId: CHILD, subagent: child })).toEqual(reduced);
      expect(managedAuthorizationForRouting(storage, bindings, ROOT, CHILD, SOURCE_TURN)).toEqual(reduced);
      expect(managedAuthorizationForRouting(storage, bindings, OTHER_ROOT, CHILD, SOURCE_TURN)).toBeUndefined();
      expect(managedAuthorizationForToolContext(bindings, OTHER_ROOT, account, { sessionId: CHILD, subagent: child })).toBeUndefined();
      // Grandchildren inherit the same bound authority.
      bind(storage, bindings, descriptor("1.1", "1", GRANDCHILD), SOURCE_TURN, () => account);
      expect(JSON.parse(bindings.authorizations.get(GRANDCHILD)!.authorization_json)).toEqual(reduced);
    });
  });

  it("retains existing bindings and lifecycle guards unchanged", async () => {
    await withSession(async (storage, bindings) => {
      const child = descriptor("1", null, CHILD);
      bind(storage, bindings, child, SOURCE_TURN, () => reduced);
      const resolver = vi.fn(() => account);
      // A retained binding returns early: no re-resolution, no widening.
      bind(storage, bindings, child, SOURCE_TURN, resolver);
      expect(resolver).not.toHaveBeenCalled();
      expect(JSON.parse(bindings.authorizations.get(CHILD)!.authorization_json)).toEqual(reduced);
      expect(() => bind(storage, bindings, child, "other-turn", resolver)).toThrow("conflicts with live authorization");
      expect(() => bind(storage, bindings, { ...child, task: "changed" }, SOURCE_TURN, resolver)).toThrow("conflicts with live authorization");
      expect(() => bind(storage, bindings, descriptor("1", null, SIBLING), SOURCE_TURN, () => account)).toThrow("identity conflicts");
      expect(() => bind(storage, bindings, descriptor("2", null, ROOT), SOURCE_TURN, () => account)).toThrow();
      expect(bindings.authorizations.has(SIBLING)).toBe(false);
      expect(applyManagedSubagentLifecycle(storage, bindings, {
        type: "status", rootSessionId: ROOT, sessionId: CHILD, descriptor: child,
        hostContextRef: SOURCE_TURN, status: { state: "completed" },
      })?.sessionId).toBe(CHILD);
      expect(() => applyManagedSubagentLifecycle(storage, bindings, {
        type: "release", rootSessionId: ROOT, sessionId: CHILD, hostContextRef: "other-turn",
      })).toThrow("does not match");
      applyManagedSubagentLifecycle(storage, bindings, { type: "release", rootSessionId: ROOT, sessionId: CHILD, hostContextRef: SOURCE_TURN });
      expect(bindings.authorizations.has(CHILD)).toBe(false);
    });
  });

  it("adopts only the native route for restored imported lineages", async () => {
    await withSession(async (storage, bindings) => {
      const child = descriptor("1", null, CHILD);
      bind(storage, bindings, child, SOURCE_TURN, () => reduced);
      bind(storage, bindings, descriptor("1.1", "1", GRANDCHILD), SOURCE_TURN);
      // Never on an agent that did not adopt imported history.
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, CHILD)).toBeUndefined();
      adopt(storage);
      expect(adoptImportedSubagentRoute(storage, bindings, OTHER_ROOT, CHILD)).toBeUndefined();
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, SIBLING)).toBeUndefined();
      // A nested child needs its parent's native route first.
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, GRANDCHILD)).toBeUndefined();
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, CHILD)).toMatchObject({ parentSessionId: ROOT, hostContextRef: SOURCE_TURN, route: null });
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, CHILD)).toBeUndefined();
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, GRANDCHILD)).toMatchObject({ parentSessionId: CHILD, hostContextRef: SOURCE_TURN, route: null });
      expect(managedAuthorizationForRouting(storage, bindings, ROOT, GRANDCHILD, SOURCE_TURN)).toEqual(reduced);
    });
    await withSession(async (storage, bindings) => {
      adopt(storage);
      // Children of a routed or cross-harness parent never inherit a native route.
      bind(storage, bindings, descriptor("1", null, CHILD), SOURCE_TURN, () => account);
      bindings.routes.set(CHILD, { routeId: "claude", parentSessionId: ROOT, hostContextRef: SOURCE_TURN, route: null, claudeModel: "claude-sonnet-4-6" });
      bind(storage, bindings, descriptor("1.1", "1", GRANDCHILD), SOURCE_TURN);
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, GRANDCHILD)).toBeUndefined();
      // Locally spawned children (destination turn present) are never adopted.
      insertTurn(storage, "local-turn", account);
      bind(storage, bindings, descriptor("2", null, SIBLING), "local-turn");
      expect(adoptImportedSubagentRoute(storage, bindings, ROOT, SIBLING)).toBeUndefined();
    });
  });

  it("keeps the spawning-turn path for local turns even when a fallback exists", async () => {
    await withSession(async (storage, bindings) => {
      insertTurn(storage, "local-turn", reduced);
      const resolver = vi.fn(() => account);
      bind(storage, bindings, descriptor("1", null, CHILD), "local-turn", resolver);
      expect(resolver).not.toHaveBeenCalled();
      expect(JSON.parse(bindings.authorizations.get(CHILD)!.authorization_json)).toEqual(reduced);
    });
  });
});

async function withSession(run: (storage: DurableObjectStorage, bindings: ManagedSubagentBindings) => void | Promise<void>): Promise<void> {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
    state.storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_turns (
      id TEXT PRIMARY KEY, request_key TEXT, request_hash TEXT NOT NULL, input_json TEXT NOT NULL,
      authorization_json TEXT NOT NULL, state TEXT NOT NULL, accepted_cursor INTEGER NOT NULL,
      may_have_inner_operation INTEGER NOT NULL DEFAULT 1, attempt_count INTEGER NOT NULL DEFAULT 0,
      retry_at INTEGER, created_at INTEGER NOT NULL, accepted_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
    )`);
    state.storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_subagent_bindings (
      kind TEXT NOT NULL, session_id TEXT NOT NULL, value_json TEXT NOT NULL, PRIMARY KEY (kind, session_id))`);
    state.storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_portability_restoration (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1), source_storage_id TEXT NOT NULL,
      events_digest TEXT NOT NULL, realtime_digest TEXT NOT NULL, turn_receipts_digest TEXT NOT NULL)`);
    await run(state.storage, new ManagedSubagentBindings(state.storage));
  });
}

function seedRoot(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_agent (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), session_id TEXT NOT NULL UNIQUE)`);
  storage.sql.exec("INSERT OR REPLACE INTO nanocodex_cloudflare_agent (singleton, session_id) VALUES (1, ?)", ROOT);
}

function adopt(storage: DurableObjectStorage): void {
  storage.sql.exec("INSERT OR REPLACE INTO managed_portability_restoration VALUES (1, 'source', 'e', 'r', 't')");
}

function insertTurn(storage: DurableObjectStorage, id: string, authorization: unknown): void {
  const now = Date.now();
  storage.sql.exec(
    `INSERT INTO managed_turns (id, request_hash, input_json, authorization_json, state, accepted_cursor,
       may_have_inner_operation, attempt_count, created_at, accepted_at, updated_at)
     VALUES (?, ?, '"input"', ?, 'accepted', 0, 0, 0, ?, ?, ?)`,
    id, `hash-${id}`, JSON.stringify(authorization), now, now, now,
  );
}

function descriptor(agentId: string, parentAgentId: string | null, sessionId: string) {
  return Object.freeze({ agentId, parentAgentId, sessionId, role: "worker", task: `task ${agentId}` as string });
}

function bind(
  storage: DurableObjectStorage,
  bindings: ManagedSubagentBindings,
  child: ReturnType<typeof descriptor>,
  hostContextRef: string,
  importedAuthority?: (root: string) => typeof account | typeof reduced | undefined,
): void {
  storage.transactionSync(() => applyManagedSubagentLifecycle(storage, bindings, {
    type: "bind", rootSessionId: ROOT, sessionId: child.sessionId, descriptor: child, hostContextRef,
  }, importedAuthority as never));
}
