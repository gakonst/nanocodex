import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import {
  applyManagedSubagentLifecycle,
  managedAuthorizationForToolContext,
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
  it("snapshots direct authority, inherits it for nested agents, and reconstructs exactly", async () => {
    await withSession(async (state) => {
      insertTurn(state.storage, "account-turn", account);
      const direct = descriptor("1", null, ACCOUNT_SESSION, "account child");
      bind(state.storage, "bind", direct, "account-turn");

      expect(authorization(state.storage, direct, connect)).toEqual(account);

      const nested = descriptor("2", "1", NESTED_SESSION, "nested child");
      bind(state.storage, "bind", nested, "account-turn");
      expect(authorization(state.storage, nested, connect)).toEqual(account);
      bind(state.storage, "reconstruct", nested, "account-turn");
      expect(state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_subagent_authorizations",
      ).one().count).toBe(2);

      expect(() => bind(state.storage, "reconstruct", {
        ...nested,
        task: "changed task",
      }, "account-turn")).toThrow("conflicts");
      expect(() => bind(state.storage, "bind", descriptor(
        "orphan", "missing", crypto.randomUUID(), "orphan",
      ), "account-turn")).toThrow("parent is missing");
    });
  });

  it("keeps a Connect child denied after a later account root and deletes only an exact ref", async () => {
    await withSession(async (state) => {
      insertTurn(state.storage, "connect-turn", connect);
      const child = descriptor("3", null, CONNECT_SESSION, "connect child");
      bind(state.storage, "bind", child, "connect-turn");

      expect(authorization(state.storage, child, account)).toEqual(connect);
      expect(managedAuthorizationForToolContext(
        state.storage,
        ROOT_SESSION,
        account,
        { ...context(ROOT_SESSION), subagent: child },
      )).toBeUndefined();
      expect(() => release(state.storage, child.sessionId, "wrong-turn")).toThrow("does not match");
      release(state.storage, child.sessionId, "connect-turn");
      expect(authorization(state.storage, child, account)).toBeUndefined();
    });
  });
});

async function withSession(
  run: (state: DurableObjectState) => void | Promise<void>,
): Promise<void> {
  const sessions = (env as unknown as {
    NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  }).NANOCODEX_SESSIONS;
  const stub = sessions.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (_session, state) => run(state));
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
  return Object.freeze({ agentId, parentAgentId, sessionId, role: "worker", task });
}

function bind(
  storage: DurableObjectStorage,
  type: "bind" | "reconstruct",
  child: ReturnType<typeof descriptor>,
  hostContextRef: string,
): void {
  storage.transactionSync(() => applyManagedSubagentLifecycle(storage, {
    type,
    rootSessionId: ROOT_SESSION,
    sessionId: child.sessionId,
    descriptor: child,
    hostContextRef,
  }));
}

function release(storage: DurableObjectStorage, sessionId: string, hostContextRef: string): void {
  storage.transactionSync(() => applyManagedSubagentLifecycle(storage, {
    type: "release",
    rootSessionId: ROOT_SESSION,
    sessionId,
    hostContextRef,
  }));
}

function authorization(
  storage: DurableObjectStorage,
  child: ReturnType<typeof descriptor>,
  active: typeof account | typeof connect,
) {
  return managedAuthorizationForToolContext(
    storage,
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
    model: "gpt-5.6-sol",
    signal: new AbortController().signal,
  };
}
