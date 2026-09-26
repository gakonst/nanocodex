import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { AsyncJobs, TypedIngestionUnavailable, UNREAL_RUNNING_OUTPUT, type FinalToolResultIntent } from "../src/asyncJobs";
import { ToolTiming } from "../src/toolTiming";
import type { NamedTool, ToolContext } from "nanocodex";

const context = (id: string): ToolContext => ({ callId: id, parentCallId: "", sessionId: "s",
  turnId: "turn-1", model: "test", signal: new AbortController().signal });
const jobId = (state: DurableObjectState, call: string) => state.storage.sql.exec<{ id: string }>(
  "SELECT id FROM async_jobs WHERE call_id = ?", call).toArray()[0]!.id;
const stub = () => (env as unknown as { SESSIONS: DurableObjectNamespace })
  .SESSIONS.getByName(`jobs-test:${crypto.randomUUID()}`);

it("keys background jobs by stable turn+call, caps active jobs, and never admits a mutating tool", async () => {
  await runInDurableObject(stub(), (_session, state) => {
    const read: NamedTool = { name: "web__run", description: "test read", handler: () => new Promise(() => {}) };
    const jobs = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn",
      async () => {}, () => {});
    const handler = jobs.tool(read).handler;
    expect(handler({ q: "stable" }, context("call-1"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-1") };
    expect(handler({ q: "stable" }, context("call-1"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    expect(() => handler({ q: "changed" }, context("call-1"))).toThrow("async invocation conflict");
    for (let n = 2; n <= 8; n++) expect(handler({ q: `q${n}` }, context(`call-${n}`))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    expect(() => handler({ q: "over capacity" }, context("call-9"))).toThrow("capacity reached");
    expect(() => jobs.tool({ name: "exec_command", description: "mutable", handler: () => "" }))
      .toThrow("not allowlisted");
    expect(jobs.status(first.job_id)).toMatchObject({ job_id: first.job_id, tool: "web__run", state: "queued" });
    const restored = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn",
      async () => {}, () => {});
    expect(restored.status(first.job_id)).toMatchObject({ job_id: first.job_id, tool: "web__run" });
    expect(restored.tool(read).handler({ q: "stable" }, context("call-1")))
      .toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const timing = new ToolTiming(state.storage.sql);
    timing.observe("internal-a", "external-a", "tool.call", { call_id: "same", tool: "web__run" }, Date.now());
    timing.observe("internal-b", "external-b", "tool.call", { call_id: "same", tool: "web__run" }, Date.now());
    expect(timing.externalTurn(context("same"))).toBeUndefined(); // refuse ambiguous cross-turn attribution
  });
});

it("persists same-call identity before egress and emits a stable terminal intent", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    const injected: FinalToolResultIntent[] = [];
    let calls = 0;
    const read: NamedTool = { name: "web__run", description: "test read", handler: () => {
      calls += 1;
      const row = state.storage.sql.exec<{ state: string; call_id: string }>(
        "SELECT state, call_id FROM async_jobs").toArray()[0];
      expect(row).toMatchObject({ state: "running", call_id: "call-1" });
      return { citation: "https://example.org/source" };
    } };
    const jobs = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn",
      async result => { injected.push(result); }, work => { tasks.push(work); });
    expect(jobs.tool(read).handler({ q: "stable" }, context("call-1"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-1") };
    expect(first.job_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(calls).toBe(0); // work must not execute inline before provisional output
    await jobs.reconcile(); // alarm resumes work after the model tool handler returns
    await Promise.all(tasks);
    expect(calls).toBe(1);
    expect(jobs.status(first.job_id)).toMatchObject({ state: "completed" });
    await jobs.reconcile(); // Fast result arrived before the next model request.
    expect(injected).toEqual([{ originalTurn: "original-turn", executionTurn: "turn-1",
      callId: "call-1", tool: "web__run", jobId: first.job_id, terminalState: "completed",
      output: '{"citation":"https://example.org/source"}' }]);
    expect(jobs.status(first.job_id)).toMatchObject({ state: "delivered" });
    await jobs.reconcile();
    expect(injected).toHaveLength(1);
  });
});

it("retries the identical terminal intent after uncertain delivery", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    const injected: FinalToolResultIntent[] = [];
    let attempts = 0;
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "2026-09-26T00:00:00Z" }) };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async result => {
        injected.push(result);
        if (++attempts === 1) throw new Error("uncertain core acknowledgement");
      }, work => { tasks.push(work); });
    expect(jobs.tool(read).handler({}, context("call-time"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-time") };
    await jobs.reconcile();
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(first.job_id)).toMatchObject({ state: "completed" });
    await jobs.reconcile();
    expect(jobs.status(first.job_id)).toMatchObject({ state: "delivered" });
    expect(injected).toHaveLength(2);
    expect(injected[0]).toEqual(injected[1]); // core deduplication required
    expect(injected[0]!.callId).toBe("call-time");
  });
});

it("holds a terminal intent without a typed ingestion adapter or a synthetic continuation", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "now" }) };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async () => { throw new TypedIngestionUnavailable(); }, work => { tasks.push(work); });
    expect(jobs.tool(read).handler({}, context("call-time"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-time") };
    await jobs.reconcile();
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(first.job_id)).toMatchObject({ state: "awaiting_integration", result: '{"utc":"now"}' });
    expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM async_jobs WHERE state = 'delivered'")
      .toArray()[0]!.n).toBe(0);
    // Reconstructed jobs retain the original call identity and output; no
    // generated user turn or provider call ID can claim a completed delivery.
    const restored = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async () => { throw new TypedIngestionUnavailable(); }, () => {});
    await restored.reconcile();
    expect(restored.status(first.job_id)).toMatchObject({ state: "awaiting_integration" });
  });
});

it("quarantines old tagged-continuation rows instead of forging a typed result", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec(`CREATE TABLE async_jobs (
      id TEXT PRIMARY KEY, invocation TEXT NOT NULL UNIQUE, original_turn TEXT NOT NULL,
      continuation_turn TEXT NOT NULL, tool TEXT NOT NULL, args TEXT NOT NULL,
      state TEXT NOT NULL, result TEXT, terminal_state TEXT, attempts INTEGER NOT NULL DEFAULT 0,
      started_at INTEGER, created_at INTEGER NOT NULL
    )`);
    state.storage.sql.exec(`INSERT INTO async_jobs
      (id, invocation, original_turn, continuation_turn, tool, args, state, created_at)
      VALUES ('old', 'old-turn:old-call', 'old-turn', 'new-user-turn', 'current_time', '{}', 'completed', 1)`);
    let injected = false;
    const jobs = new AsyncJobs(state.storage, {}, () => "old-turn",
      async () => { injected = true; }, () => {});
    await jobs.reconcile();
    expect(jobs.status("old")).toMatchObject({ state: "legacy_uninjectable" });
    expect(injected).toBe(false);
    // Migration can still write new intents despite the old NOT NULL column;
    // that column must never become a continuation turn.
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "now" }) };
    const migrated = new AsyncJobs(state.storage, { current_time: read }, () => "new-turn",
      async () => { throw new TypedIngestionUnavailable(); }, () => {});
    expect(migrated.tool(read).handler({}, context("new-call"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const created = { job_id: jobId(state, "new-call") };
    expect(created.job_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(state.storage.sql.exec<{ continuation_turn: string }>(
      "SELECT continuation_turn FROM async_jobs WHERE id = ?", created.job_id).toArray()[0]?.continuation_turn).toBe("");
  });
});

it("fences stale results from a crashed lease and delivers the winning retry once", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    let resolveFirst!: (value: unknown) => void;
    const firstResult = new Promise<unknown>(resolve => { resolveFirst = resolve; });
    let attempts = 0;
    const read: NamedTool = { name: "current_time", description: "read", handler: () => {
      return ++attempts === 1 ? firstResult : { fresh: true };
    } };
    const sent: FinalToolResultIntent[] = [];
    const deliver = async (result: FinalToolResultIntent) => { sent.push(result); };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn", deliver,
      work => { tasks.push(work); });
    expect(jobs.tool(read).handler({}, context("call-restarted"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const id = jobId(state, "call-restarted");
    expect(attempts).toBe(0);
    await jobs.reconcile();
    expect(attempts).toBe(1);
    // A second instance starts after a lease expires while the first promise
    // is still in flight, just as a stale worker could finish after recovery.
    state.storage.sql.exec("UPDATE async_jobs SET started_at = ? WHERE id = ?", Date.now() - 31_000, id);
    const restored = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn", deliver,
      work => { tasks.push(work); });
    await restored.reconcile();
    expect(attempts).toBe(2);
    await Promise.resolve();
    expect(restored.status(id)).toMatchObject({ state: "completed", result: '{"fresh":true}' });
    resolveFirst({ stale: true });
    await Promise.all(tasks);
    expect(restored.status(id)).toMatchObject({ state: "completed", result: '{"fresh":true}' });
    await restored.reconcile();
    await restored.reconcile();
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ callId: "call-restarted", output: '{"fresh":true}' });
  });
});
