import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";

import {
  applyManagedSubagentLifecycle,
  ManagedSubagentBindings,
  initializeManagedSubagentBindings,
  managedAuthorizationForToolContext,
  managedAuthorizationForRouting,
  type DurableAgentSession,
} from "../src/index";

const ROOT_SESSION = "01992222-2222-7222-8222-222222222222";
const ACCOUNT_SESSION = "01993333-3333-7333-8333-333333333333";
const CONNECT_SESSION = "01994444-4444-7444-8444-444444444444";
const NESTED_SESSION = "01995555-5555-7555-8555-555555555555";
const account = { capabilities: ["agents:write", "tools:use"] as const };
const connect = {
  capabilities: ["agents:write", "tools:use"] as const,
  connectGrant: {
    grantId: `0x${"a".repeat(64)}`,
    connectors: ["chatgpt"] as const,
    mcpIds: [] as const,
  },
};

describe("managed subagent authorization ownership", () => {
  it("discards obsolete child metadata without touching root turns or routes", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "account-turn", account);
      state.storage.sql.exec("INSERT INTO managed_thread_route VALUES (1, '{}')");
      for (const table of ["managed_subagent_authorizations", "managed_subagent_routes"]) {
        state.storage.sql.exec(`CREATE TABLE ${table} (legacy TEXT)`);
        state.storage.sql.exec(`INSERT INTO ${table} VALUES ('obsolete child')`);
      }
      initializeManagedSubagentBindings(state.storage);
      initializeManagedSubagentBindings(state.storage);
      expect(state.storage.sql.exec("SELECT name FROM sqlite_master WHERE name IN ('managed_subagent_authorizations', 'managed_subagent_routes')").toArray()).toEqual([]);
      expect(state.storage.sql.exec("SELECT id FROM managed_turns").toArray()).toEqual([{ id: "account-turn" }]);
      expect(state.storage.sql.exec("SELECT route_json FROM managed_thread_route").one()).toEqual({ route_json: "{}" });
    });
  });

  it("resolves routing authority from exact parent provenance, independently of current root authority", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "account-turn", account);
      insertTurn(state.storage, "connect-turn", connect);
      const direct = descriptor("route-parent", null, ACCOUNT_SESSION, "parent task");
      bind(state.storage, bindings, direct, "account-turn");
      const routeAuthorization = (parent: string, ref: string, root = ROOT_SESSION) =>
        managedAuthorizationForRouting(state.storage, bindings, root, parent, ref);
      expect(routeAuthorization(ROOT_SESSION, "account-turn")).toEqual(account);
      expect(routeAuthorization(ROOT_SESSION, "connect-turn")).toEqual(connect);
      expect(routeAuthorization(ACCOUNT_SESSION, "account-turn")).toEqual(account);
      expect(routeAuthorization(ACCOUNT_SESSION, "connect-turn")).toBeUndefined();
      expect(routeAuthorization(ACCOUNT_SESSION, "account-turn", CONNECT_SESSION)).toBeUndefined();
      expect(routeAuthorization(NESTED_SESSION, "account-turn")).toBeUndefined();
      const nested = descriptor("route-nested", "route-parent", NESTED_SESSION, "nested task");
      bind(state.storage, bindings, nested, "account-turn");
      expect(routeAuthorization(NESTED_SESSION, "account-turn")).toEqual(account);
      release(state.storage, bindings, ACCOUNT_SESSION, "account-turn");
      expect(routeAuthorization(ACCOUNT_SESSION, "account-turn")).toBeUndefined();
      // A retained descendant owns its snapshot even after its parent finishes.
      expect(routeAuthorization(NESTED_SESSION, "account-turn")).toEqual(account);
    });
  });

  it("snapshots direct authority and inherits it only within the live runtime", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "account-turn", account);
      const direct = descriptor("1", null, ACCOUNT_SESSION, "account child");
      bind(state.storage, bindings, direct, "account-turn");

      expect(authorization(bindings, direct, connect)).toEqual(account);

      const nested = descriptor("2", "1", NESTED_SESSION, "nested child");
      bind(state.storage, bindings, nested, "account-turn");
      expect(authorization(bindings, nested, connect)).toEqual(account);
      bind(state.storage, bindings, nested, "account-turn");
      expect(bindings.authorizations.size).toBe(2);

      expect(() => bind(state.storage, bindings, {
        ...nested,
        task: "changed task",
      }, "account-turn")).toThrow("conflicts");
      expect(() => bind(state.storage, bindings, descriptor(
        "orphan", "missing", crypto.randomUUID(), "orphan",
      ), "account-turn")).toThrow("parent is missing");
    });
  });

  it("never restores child authority from durable root turns", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "account-turn", account);
      const child = descriptor("existing", null, ACCOUNT_SESSION, "existing task");
      bind(state.storage, bindings, child, "account-turn");
      const replacement = new ManagedSubagentBindings();
      expect(authorization(replacement, child, account)).toBeUndefined();
      expect(managedAuthorizationForRouting(state.storage, replacement, ROOT_SESSION, ACCOUNT_SESSION, "account-turn")).toBeUndefined();
      expect(managedAuthorizationForRouting(state.storage, replacement, ROOT_SESSION, ROOT_SESSION, "account-turn")).toEqual(account);
    });
  });

  it("retains only identity digests for large task and role content", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "account-turn", account);
      const child = { ...descriptor("large", null, ACCOUNT_SESSION, "x".repeat(2 * 1024 * 1024)), role: "r".repeat(2 * 1024 * 1024) };
      bind(state.storage, bindings, child, "account-turn");
      expect(authorization(bindings, child, account)).toEqual(account);
      bind(state.storage, bindings, child, "account-turn");
      expect(bindings.authorizations.get(ACCOUNT_SESSION)).toMatchObject({
        role: expect.stringMatching(/^[a-f0-9]{64}$/), task: expect.stringMatching(/^[a-f0-9]{64}$/),
      });
      expect(authorization(bindings, { ...child, task: child.task + "different" }, account)).toBeUndefined();
      expect(authorization(bindings, { ...child, role: child.role + "different" }, account)).toBeUndefined();
    });
  });

  it("keeps a Connect child denied after a later account root and deletes only an exact ref", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "connect-turn", connect);
      const child = descriptor("3", null, CONNECT_SESSION, "connect child");
      bind(state.storage, bindings, child, "connect-turn");

      expect(authorization(bindings, child, account)).toEqual(connect);
      expect(managedAuthorizationForToolContext(
        bindings,
        ROOT_SESSION,
        account,
        { ...context(ROOT_SESSION), subagent: child },
      )).toBeUndefined();
      expect(() => release(state.storage, bindings, child.sessionId, "wrong-turn")).toThrow("does not match");
      release(state.storage, bindings, child.sessionId, "connect-turn");
      expect(authorization(bindings, child, account)).toBeUndefined();
    });
  });

  it("rejects reconstruction and missing provenance", async () => {
    await withSession(async (state, bindings) => {
      const child = descriptor("legacy", null, ACCOUNT_SESSION, "legacy child");
      for (const type of ["reconstruct", "bind", "release"]) {
        expect(() => applyManagedSubagentLifecycle(state.storage, bindings, {
          type, rootSessionId: ROOT_SESSION, sessionId: child.sessionId, descriptor: child,
        })).toThrow("invalid managed subagent lifecycle event");
      }
    });
  });
});

async function withSession(
  run: (state: DurableObjectState, bindings: ManagedSubagentBindings) => void | Promise<void>,
): Promise<void> {
  const sessions = (env as unknown as {
    NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  }).NANOCODEX_SESSIONS;
  const stub = sessions.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (_session, state) => {
    // A fresh object initializes managed storage only on create; seed the
    // tables these authorization paths read.
    state.storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_turns (
      id TEXT PRIMARY KEY, request_key TEXT, request_hash TEXT NOT NULL, input_json TEXT NOT NULL,
      authorization_json TEXT NOT NULL, state TEXT NOT NULL, accepted_cursor INTEGER NOT NULL,
      may_have_inner_operation INTEGER NOT NULL DEFAULT 1, attempt_count INTEGER NOT NULL DEFAULT 0,
      retry_at INTEGER, created_at INTEGER NOT NULL, accepted_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
    )`);
    state.storage.sql.exec("CREATE TABLE IF NOT EXISTS managed_thread_route (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), route_json TEXT NOT NULL)");
    initializeManagedSubagentBindings(state.storage);
    await run(state, new ManagedSubagentBindings());
  });
}

function insertTurn(storage: DurableObjectStorage, id: string, authorization: unknown): void {
  const now = Date.now();
  storage.sql.exec(
    `INSERT INTO managed_turns (
       id, request_hash, input_json, authorization_json, state, accepted_cursor,
       may_have_inner_operation, attempt_count, created_at, accepted_at, updated_at
     ) VALUES (?, ?, '"input"', ?, 'accepted', 0, 0, 0, ?, ?, ?)`,
    id,
    `hash-${id}`,
    JSON.stringify(authorization),
    now,
    now,
    now,
  );
}

function descriptor(
  agentId: string,
  parentAgentId: string | null,
  sessionId: string,
  task: string,
) {
  return Object.freeze({ agentId, parentAgentId, sessionId, role: "worker" as string, task });
}

function bind(
  storage: DurableObjectStorage,
  bindings: ManagedSubagentBindings,
  child: ReturnType<typeof descriptor>,
  hostContextRef: string,
): void {
  storage.transactionSync(() => applyManagedSubagentLifecycle(storage, bindings, {
    type: "bind",
    rootSessionId: ROOT_SESSION,
    sessionId: child.sessionId,
    descriptor: child,
    hostContextRef,
  }));
}

function release(storage: DurableObjectStorage, bindings: ManagedSubagentBindings, sessionId: string, hostContextRef: string): void {
  storage.transactionSync(() => applyManagedSubagentLifecycle(storage, bindings, {
    type: "release",
    rootSessionId: ROOT_SESSION,
    sessionId,
    hostContextRef,
  }));
}

function authorization(
  bindings: ManagedSubagentBindings,
  child: ReturnType<typeof descriptor>,
  active: typeof account | typeof connect,
) {
  return managedAuthorizationForToolContext(
    bindings,
    ROOT_SESSION,
    active,
    { ...context(child.sessionId), subagent: child },
  );
}

function context(sessionId: string) {
  return {
    callId: "call",
    parentCallId: "cell",
    sessionId,
    model: "gpt-6.1-sol",
    signal: new AbortController().signal,
  };
}

describe("durable managed subagent bindings", () => {
  it("survive a runtime rebuild and clear on release", async () => {
    await withSession(async (state) => {
      initializeManagedSubagentBindings(state.storage);
      const first = new ManagedSubagentBindings(state.storage);
      first.routes.set("child-1", { routeId: "r", parentSessionId: "p", hostContextRef: "h", route: null, claudeModel: "claude-haiku-4-5" });
      const rebuilt = new ManagedSubagentBindings(state.storage);
      expect(rebuilt.routes.get("child-1")?.claudeModel).toBe("claude-haiku-4-5");
      rebuilt.routes.delete("child-1");
      expect(new ManagedSubagentBindings(state.storage).routes.size).toBe(0);
    });
  });
});

describe("managed subagent status after its live binding changed", () => {
  it("drops stale statuses with one warning per child and still rejects foreign authority", async () => {
    await withSession(async (state, bindings) => {
      insertTurn(state.storage, "account-turn", account);
      insertTurn(state.storage, "later-turn", account);
      const warnings: unknown[] = [];
      const warn = vi.spyOn(console, "warn").mockImplementation((entry) => { warnings.push(entry); });
      try {
        const child = descriptor("41", null, ACCOUNT_SESSION, "status child");
        const status = (state_: string, hostContextRef = "account-turn", root = ROOT_SESSION, child_ = child) =>
          applyManagedSubagentLifecycle(state.storage, bindings, {
            type: "status", rootSessionId: root, sessionId: child_.sessionId,
            descriptor: child_, hostContextRef, status: state_ === "completed" ? { state: state_, output: "done" } : { state: state_ },
          });
        bind(state.storage, bindings, child, "account-turn");
        expect(status("running")).toBeUndefined();
        expect(status("completed")).toMatchObject({ sessionId: ACCOUNT_SESSION, host_context_ref: "account-turn" });
        // A status that names a different spawning turn is observational only:
        // it neither throws nor wakes the root through the retained binding.
        expect(status("completed", "later-turn")).toBeUndefined();
        // Genuine authority mismatches are still rejected, with a stable code.
        expect(() => status("running", "account-turn", CONNECT_SESSION)).toThrow(expect.objectContaining({
          code: "subagent_status_authority_mismatch",
          message: "managed subagent status does not match live authorization",
        }));
        expect(() => status("running", "account-turn", ROOT_SESSION, descriptor("41", null, ACCOUNT_SESSION, "replaced task")))
          .toThrow(expect.objectContaining({ code: "subagent_status_authority_mismatch" }));
        expect(() => status("paused")).toThrow(expect.objectContaining({ code: "invalid_subagent_status" }));
        release(state.storage, bindings, ACCOUNT_SESSION, "account-turn");
        // Released children keep reporting until their runtime drops them,
        // including from a rebuilt runtime that restores the same tree.
        for (const next of ["running", "interrupted", "failed", "closed"]) expect(status(next)).toBeUndefined();
        const rebuilt = new ManagedSubagentBindings(state.storage);
        for (const next of ["running", "completed"]) {
          expect(applyManagedSubagentLifecycle(state.storage, rebuilt, {
            type: "status", rootSessionId: ROOT_SESSION, sessionId: child.sessionId, descriptor: child,
            hostContextRef: "account-turn", status: { state: next, output: "done" },
          })).toBeUndefined();
        }
        expect(warnings).toEqual([
          { type: "managed.subagent_status_dropped", reason: "host_context_superseded", root_session_id: ROOT_SESSION, session_id: ACCOUNT_SESSION },
          { type: "managed.subagent_status_dropped", reason: "binding_missing", root_session_id: ROOT_SESSION, session_id: ACCOUNT_SESSION },
        ]);
        expect(JSON.stringify(warnings)).not.toContain("status child");
      } finally { warn.mockRestore(); }
    });
  });
});
