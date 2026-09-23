import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { type Env, DurableAgentSession } from "../src/index";

it.each([undefined, "false", "TRUE"])("keeps native SQL without the explicit audit opt-in (%s)", async (trace) => {
  const sessions = (env as unknown as Env).NANOCODEX_SESSIONS as DurableObjectNamespace<DurableAgentSession>;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_instance, original) => {
    const logs = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      const session = new DurableAgentSession(original, { ...env, NANOCODEX_PERFORMANCE_TRACE: trace } as unknown as Env);
      expect(session.computerContext).toBe(original);
      expect(session.computerContext.storage.sql).toBe(original.storage.sql);
      expect(session.computerContext.storage.sql.exec("SELECT ? AS value", "private-binding").one()).toEqual({ value: "private-binding" });
      await Promise.resolve();
      expect(logs.mock.calls.some(call => call[0]?.type === "managed.sql_batch")).toBe(false);
    } finally { logs.mockRestore(); await original.storage.deleteAlarm(); }
  });
});

it("preserves native SQL cursors, receivers, bindings and transaction rollback while auditing", async () => {
  const sessions = (env as unknown as Env).NANOCODEX_SESSIONS as DurableObjectNamespace<DurableAgentSession>;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_instance, original) => {
    const logs = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      const session = new DurableAgentSession(original, { ...env, NANOCODEX_PERFORMANCE_TRACE: "true" } as unknown as Env);
      const state = session.computerContext;
      expect(state).not.toBe(original);
      state.storage.sql.exec("CREATE TABLE audit_test (id INTEGER PRIMARY KEY, value TEXT)");
      state.storage.sql.exec("INSERT INTO audit_test VALUES (?, ?)", 1, "private-binding");
      expect(state.storage.sql.exec("SELECT * FROM audit_test").one()).toEqual({ id: 1, value: "private-binding" });
      expect([...state.storage.sql.exec("SELECT id FROM audit_test")]).toEqual([{ id: 1 }]);
      expect(state.storage.sql.exec("SELECT id FROM audit_test").toArray()).toEqual([{ id: 1 }]);
      expect([...state.storage.sql.exec("SELECT id FROM audit_test").raw()]).toEqual([[1]]);
      expect(() => state.storage.transactionSync(() => {
        state.storage.sql.exec("INSERT INTO audit_test VALUES (2, 'private-literal')");
        throw Error("rollback");
      })).toThrow("rollback");
      expect(state.storage.sql.exec("SELECT COUNT(*) AS n FROM audit_test").one().n).toBe(1);
      await state.storage.put("audit-key", "value");
      expect(await state.storage.get("audit-key")).toBe("value");
      await Promise.resolve();
      const records = logs.mock.calls.map(call => call[0]).filter(record => record?.type === "managed.sql_batch")
        .flatMap(record => record.statements).filter(record => record.tables.includes("audit_test"));
      expect(records.reduce((sum, record) => sum + record.count, 0)).toBe(8);
      expect(records.some(record => record.rows_read > 0)).toBe(true);
      expect(records.some(record => record.rows_written > 0)).toBe(true);
      expect(JSON.stringify(records)).not.toContain("private-binding");
      expect(JSON.stringify(records)).not.toContain("private-literal");
    } finally { logs.mockRestore(); await original.storage.deleteAlarm(); }
  });
});

it("logs only bounded socket timing fields with the pinned managed session ID", async () => {
  const { performanceSocketTiming } = await import("../src/performance");
  const logs = vi.spyOn(console, "info").mockImplementation(() => {});
  const observation = { message_count: 3, delivered_message_count: 2, buffered_message_count: 1,
    discarded_message_count: 1, queue_residence_total_ms: 10, queue_residence_max_ms: 10,
    session_id: "untrusted-session", headers: { authorization: "private" }, body: "private",
    provider_timings: [
      { response_id: "resp_fixture", pre_inference_ms: 30, engine_queue_max_ms: 5, engine_service_ttft_total_ms: 20, arbitrary_ms: 100, body: "private" },
      { response_id: "resp_invalid", pre_inference_ms: "private", engine_queue_max_ms: NaN, engine_service_ttft_total_ms: -1 },
      { response_id: "resp_\nprivate", pre_inference_ms: 1 },
      { pre_inference_ms: 0 },
    ],
  };
  try {
    performanceSocketTiming("managed-fixture-session", observation);
    expect(logs.mock.calls.map(call => call[0])).toEqual([
      { type: "managed.performance", stage: "transport.socket_queue", session_id: "managed-fixture-session",
        message_count: 3, delivered_message_count: 2, buffered_message_count: 1, discarded_message_count: 1,
        queue_residence_total_ms: 10, queue_residence_max_ms: 10 },
      { type: "managed.performance", stage: "transport.provider_timing", session_id: "managed-fixture-session",
        response_id: "resp_fixture", pre_inference_ms: 30, engine_queue_max_ms: 5, engine_service_ttft_total_ms: 20 },
      { type: "managed.performance", stage: "transport.provider_timing", session_id: "managed-fixture-session", pre_inference_ms: 0 },
    ]);
    expect(JSON.stringify(logs.mock.calls)).not.toMatch(/private|untrusted|arbitrary|headers|body/);
    logs.mockClear();
    for (const bad of [undefined, [], { ...observation, message_count: "private" },
      { ...observation, queue_residence_total_ms: Infinity }, { ...observation, queue_residence_max_ms: -1 }]) {
      performanceSocketTiming("managed-fixture-session", bad);
    }
    expect(logs).not.toHaveBeenCalled();
    performanceSocketTiming("managed-fixture-session", { ...observation,
      provider_timings: Array.from({ length: 40 }, (_, i) => ({ response_id: `resp_${i}`, pre_inference_ms: i })) });
    expect(logs).toHaveBeenCalledTimes(33);
    logs.mockImplementation(() => { throw new Error("logging unavailable"); });
    expect(() => performanceSocketTiming("managed-fixture-session", observation)).not.toThrow();
  } finally { logs.mockRestore(); }
});

it("observes pending creation commits without blocking or exposing storage errors", async () => {
  const { performanceCommit } = await import("../src/performance");
  const logs = vi.spyOn(console, "info").mockImplementation(() => {});
  let complete!: () => void;
  let retained: Promise<unknown> | undefined;
  const pending = new Promise<void>(resolve => { complete = resolve; });
  const sync = vi.fn(() => pending);
  const state = { id: { toString: () => "fixture-storage-id" }, storage: { sync },
    waitUntil(task: Promise<unknown>) { retained = task; } } as unknown as DurableObjectState;
  try {
    expect(performanceCommit(state, "session.create.commit")).toBeUndefined();
    expect(sync).toHaveBeenCalledOnce();
    expect(logs).not.toHaveBeenCalled();
    complete(); await retained;
    expect(logs.mock.calls[0]?.[0]).toMatchObject({ type: "managed.performance", trace_id: "fixture-storage-id",
      stage: "session.create.commit", success: true });
    sync.mockImplementation(() => Promise.reject(new Error("private storage details")));
    performanceCommit(state, "session.create.commit"); await retained;
    expect(logs.mock.calls[1]?.[0]).toMatchObject({ success: false });
    expect(JSON.stringify(logs.mock.calls)).not.toContain("private");
    logs.mockImplementation(() => { throw new Error("logging unavailable"); });
    performanceCommit(state, "session.create.commit");
    await expect(retained).resolves.toBeUndefined();
    sync.mockImplementation(() => { throw new Error("storage unavailable"); });
    expect(() => performanceCommit(state, "session.create.commit")).not.toThrow();
  } finally { logs.mockRestore(); }
});
