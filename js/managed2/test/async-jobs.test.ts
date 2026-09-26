import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { AsyncJobs, TypedIngestionUnavailable, UNREAL_RUNNING_OUTPUT, type FinalToolResultIntent, type FinalToolResultReceipt } from "../src/asyncJobs";
import { ToolTiming } from "../src/toolTiming";
import type { NamedTool, ToolContext } from "nanocodex";

const context = (id: string): ToolContext => ({ callId: id, parentCallId: "", sessionId: "s",
  turnId: "turn-1", model: "test", signal: new AbortController().signal });
const jobId = (state: DurableObjectState, call: string) => state.storage.sql.exec<{ id: string }>(
  "SELECT id FROM async_jobs WHERE call_id = ?", call).toArray()[0]!.id;
const accepted = (intent: FinalToolResultIntent, continuation_started = false): FinalToolResultReceipt => ({
  operation_id: intent.jobId, call_id: intent.callId, replayed: false, continuation_started,
});
const stub = () => (env as unknown as { SESSIONS: DurableObjectNamespace })
  .SESSIONS.getByName(`jobs-test:${crypto.randomUUID()}`);

it("keys background jobs by stable turn+call, caps active jobs, and rejects tools absent from the registered catalog", async () => {
  await runInDurableObject(stub(), (_session, state) => {
    const read: NamedTool = { name: "web__run", description: "test read", handler: () => new Promise(() => {}) };
    const jobs = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn",
      async result => accepted(result), () => {});
    const handler = jobs.tool(read).handler;
    expect(handler({ q: "stable" }, context("call-1"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-1") };
    expect(handler({ q: "stable" }, context("call-1"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    expect(() => handler({ q: "changed" }, context("call-1"))).toThrow("async invocation conflict");
    for (let n = 2; n <= 8; n++) expect(handler({ q: `q${n}` }, context(`call-${n}`))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    expect(() => handler({ q: "over capacity" }, context("call-9"))).toThrow("capacity reached");
    expect(() => jobs.tool({ name: "exec_command", description: "mutable", handler: () => "" }))
      .toThrow("not registered");
    expect(jobs.status(first.job_id)).toMatchObject({ job_id: first.job_id, tool: "web__run", state: "queued" });
    const restored = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn",
      async result => accepted(result), () => {});
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
      async result => { injected.push(result); return accepted(result); }, work => { tasks.push(work); });
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
    expect(jobs.status(first.job_id)).toMatchObject({ state: "checkpointed" });
    await jobs.reconcile();
    expect(injected).toHaveLength(1);
  });
});

it("reconciles an active acceptance once after source turn settles, including after DO rehydrate", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('original-turn', 'work', 'accepted')");
    const tasks: Promise<unknown>[] = [];
    const intents: FinalToolResultIntent[] = [];
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "now" }) };
    const deliver = async (intent: FinalToolResultIntent) => {
      intents.push(intent);
      return accepted(intent); // acceptance alone is not provider uptake
    };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      deliver, work => { tasks.push(work); });
    expect(jobs.tool(read).handler({}, context("call-cancelled"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const id = jobId(state, "call-cancelled");
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    await jobs.reconcile();
    expect(intents).toHaveLength(1); // never resubmit during the active turn
    state.storage.sql.exec("UPDATE turns SET state = 'failed' WHERE id = 'original-turn'");
    const restored = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      deliver, () => {});
    await restored.reconcile();
    expect(intents).toHaveLength(2);
    expect(intents[1]).toEqual(intents[0]); // never mint a new operation after cancellation
    expect(restored.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    await restored.reconcile();
    expect(intents).toHaveLength(2); // bounded; still not a model-uptake receipt
  });
});

it("keeps a terminal-racing active receipt eligible for idle retry", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('original-turn', 'work', 'accepted')");
    const tasks: Promise<unknown>[] = [];
    const intents: FinalToolResultIntent[] = [];
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "now" }) };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async intent => {
        intents.push(intent);
        if (intents.length === 1) await gate;
        return accepted(intent);
      }, work => { tasks.push(work); });
    jobs.tool(read).handler({}, context("call-race"));
    await Promise.all(tasks);
    const first = jobs.reconcile();
    // Wait until the first delivery has started and captured the active state.
    for (let attempt = 0; intents.length === 0 && attempt < 100; attempt++) await Promise.resolve();
    expect(intents).toHaveLength(1);
    state.storage.sql.exec("UPDATE turns SET state = 'failed' WHERE id = 'original-turn'");
    release();
    await first;
    await jobs.reconcile();
    expect(intents).toHaveLength(2);
    expect(intents[1]).toEqual(intents[0]);
  });
});

it("only a matching durable completed model-step status marks an active output delivered", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('original-turn', 'work', 'accepted')");
    const tasks: Promise<unknown>[] = [];
    const intents: FinalToolResultIntent[] = [];
    let status: string = "bound_unconfirmed";
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "now" }) };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async intent => { intents.push(intent); return accepted(intent); }, work => { tasks.push(work); },
      new Set(["current_time"]), undefined, async () => ({ state: status, model_call_index: 2,
        ...(status === "confirmed" ? { response_id: "resp-2" } : {}) }),
      async () => { if (status === "confirmed") throw new TypedIngestionUnavailable();
        return { state: "pruned_or_unknown" }; });
    jobs.tool(read).handler({}, context("call-confirmed"));
    const id = jobId(state, "call-confirmed");
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    await jobs.reconcile();
    expect(intents).toHaveLength(1);
    state.storage.sql.exec("UPDATE turns SET state = 'completed' WHERE id = 'original-turn'");
    status = "confirmed";
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "delivered", continuation_started: true });
    expect(intents).toHaveLength(1); // never resubmit a confirmed active output
  });
});

it("idle wake uptake requires the exact durable model step, not the submission hint", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('original-turn', 'work', 'completed')");
    const tasks: Promise<unknown>[] = [];
    let receipts = 0;
    let idleState: string = "accepted_unbound";
    const read: NamedTool = { name: "current_time", description: "test", handler: () => ({ utc: "now" }) };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async intent => { receipts++; return accepted(intent, true); }, task => { tasks.push(task); },
      new Set(["current_time"]), undefined,
      async () => ({ state: "pruned_or_unknown" }),
      async () => idleState === "confirmed"
        ? { state: idleState, model_call_index: 1, response_id: "wake-response" }
        : { state: idleState });
    jobs.tool(read).handler({}, context("call-idle"));
    const id = jobId(state, "call-idle");
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    await jobs.reconcile();
    expect(receipts).toBe(1);
    idleState = "bound_unconfirmed";
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed" });
    // A crash after staging but before an alarm write still re-arms from the
    // durable unconfirmed row; never strands an in-flight idle wake.
    await state.storage.deleteAlarm();
    const rearmed: Promise<unknown>[] = [];
    new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async intent => accepted(intent), task => { rearmed.push(task); },
      new Set(["current_time"]), undefined, async () => ({ state: "pruned_or_unknown" }),
      async () => ({ state: "bound_unconfirmed" }));
    await Promise.all(rearmed);
    expect(await state.storage.getAlarm()).not.toBeNull();
    idleState = "confirmed";
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "delivered", continuation_started: true });
    expect(receipts).toBe(1);
    const restarted = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async intent => { receipts++; return accepted(intent); }, () => {});
    await restarted.reconcile();
    expect(restarted.status(id)).toMatchObject({ state: "delivered" });
    expect(receipts).toBe(1);
  });
});

it("only an authoritative discarded status retries the original job at idle", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('original-turn', 'work', 'accepted')");
    const tasks: Promise<unknown>[] = [];
    const intents: FinalToolResultIntent[] = [];
    let status = "bound_unconfirmed";
    const read: NamedTool = { name: "current_time", description: "test read", handler: () => ({ utc: "now" }) };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async intent => { intents.push(intent); return accepted(intent); }, work => { tasks.push(work); },
      new Set(["current_time"]), undefined, async () => ({ state: status }));
    jobs.tool(read).handler({}, context("call-discarded"));
    const id = jobId(state, "call-discarded");
    await Promise.all(tasks);
    await jobs.reconcile();
    state.storage.sql.exec("UPDATE turns SET state = 'failed' WHERE id = 'original-turn'");
    await jobs.reconcile(); // bound is not consumed, and must not be replayed
    expect(intents).toHaveLength(1);
    // Simulate a later status discovery after a cold restart/version bump.
    state.storage.sql.exec("UPDATE async_jobs SET wake_generation = 0 WHERE id = ?", id);
    status = "discarded";
    await jobs.reconcile();
    expect(intents).toHaveLength(2);
    expect(intents[1]).toEqual(intents[0]);
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    await jobs.reconcile();
    expect(intents).toHaveLength(2);
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
        return { ...accepted(result), replayed: true };
      }, work => { tasks.push(work); });
    expect(jobs.tool(read).handler({}, context("call-time"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-time") };
    await jobs.reconcile();
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(first.job_id)).toMatchObject({ state: "completed" });
    await jobs.reconcile();
    expect(jobs.status(first.job_id)).toMatchObject({ state: "checkpointed" });
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
      async (): Promise<FinalToolResultReceipt> => { throw new TypedIngestionUnavailable(); }, work => { tasks.push(work); });
    expect(jobs.tool(read).handler({}, context("call-time"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const first = { job_id: jobId(state, "call-time") };
    await jobs.reconcile();
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(first.job_id)).toMatchObject({ state: "awaiting_integration", result: '{"utc":"now"}' });
    expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM async_jobs WHERE state = 'checkpointed'")
      .toArray()[0]!.n).toBe(0);
    // Reconstructed jobs retain the original call identity and output; no
    // generated user turn or provider call ID can claim a completed delivery.
    const restored = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async (): Promise<FinalToolResultReceipt> => { throw new TypedIngestionUnavailable(); }, () => {});
    await restored.reconcile();
    expect(restored.status(first.job_id)).toMatchObject({ state: "awaiting_integration" });
    await state.storage.deleteAlarm();
    const rearm: Promise<unknown>[] = [];
    new AsyncJobs(state.storage, { current_time: read }, () => "original-turn",
      async (): Promise<FinalToolResultReceipt> => { throw new TypedIngestionUnavailable(); },
      task => { rearm.push(task); });
    await Promise.all(rearm);
    expect(await state.storage.getAlarm()).not.toBeNull(); // next cold adapter can recover
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
      async (): Promise<FinalToolResultReceipt> => { throw new TypedIngestionUnavailable(); }, () => {});
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
    const deliver = async (result: FinalToolResultIntent) => { sent.push(result); return accepted(result); };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn", deliver,
      work => { tasks.push(work); }, new Set(["current_time"]));
    expect(jobs.tool(read).handler({}, context("call-restarted"))).toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const id = jobId(state, "call-restarted");
    expect(attempts).toBe(0);
    await jobs.reconcile();
    expect(attempts).toBe(1);
    // A second instance starts after a lease expires while the first promise
    // is still in flight, just as a stale worker could finish after recovery.
    state.storage.sql.exec("UPDATE async_jobs SET started_at = ? WHERE id = ?", Date.now() - 31_000, id);
    const restored = new AsyncJobs(state.storage, { current_time: read }, () => "original-turn", deliver,
      work => { tasks.push(work); }, new Set(["current_time"]));
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

it("never replays a mutable tool after its lease becomes uncertain", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    let resolveFirst!: (value: unknown) => void;
    const first = new Promise<unknown>(resolve => { resolveFirst = resolve; });
    let executions = 0;
    const mutate: NamedTool = { name: "exec_command", description: "test mutation", handler: () => {
      executions++;
      expect(state.storage.sql.exec<{ state: string }>("SELECT state FROM async_jobs").toArray()[0]?.state)
        .toBe("running");
      return first;
    } };
    const delivered: FinalToolResultIntent[] = [];
    const make = () => new AsyncJobs(state.storage, { exec_command: mutate }, () => "turn",
      async result => { delivered.push(result); return accepted(result); }, work => { tasks.push(work); });
    const jobs = make();
    expect(jobs.tool(mutate).handler({ cmd: "touch /brain/sentinel" }, context("call-mutable")))
      .toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const id = jobId(state, "call-mutable");
    await jobs.reconcile();
    expect(executions).toBe(1);
    state.storage.sql.exec("UPDATE async_jobs SET started_at = ? WHERE id = ?", Date.now() - 31_000, id);
    const restored = make();
    await restored.reconcile();
    expect(executions).toBe(1);
    expect(restored.status(id)).toMatchObject({ state: "uncertain" });
    await restored.reconcile();
    expect(delivered).toHaveLength(1);
    expect(delivered[0]).toMatchObject({ callId: "call-mutable", terminalState: "uncertain" });
    resolveFirst("late success");
    await Promise.all(tasks);
    expect(restored.status(id)).toMatchObject({ state: "checkpointed" });
  });
});

it("cancels a queued mutable operation without dispatching it", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    let executions = 0;
    const mutate: NamedTool = { name: "exec_command", description: "test mutation",
      handler: () => { executions++; return "unexpected"; } };
    const delivered: FinalToolResultIntent[] = [];
    const jobs = new AsyncJobs(state.storage, { exec_command: mutate }, () => "turn",
      async result => { delivered.push(result); return accepted(result); }, () => {});
    jobs.tool(mutate).handler({ cmd: "touch /brain/sentinel" }, context("queued-mutable"));
    const id = jobId(state, "queued-mutable");
    await jobs.cancel(id);
    await jobs.reconcile();
    expect(executions).toBe(0);
    expect(delivered).toHaveLength(1);
    expect(delivered[0]).toMatchObject({ terminalState: "cancelled" });
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed" });
  });
});


it("does not acknowledge a mismatched core receipt, then reconciles the stable operation after ambiguous acceptance", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    const read: NamedTool = { name: "current_time", description: "read", handler: () => ({ utc: "now" }) };
    const intents: FinalToolResultIntent[] = [];
    let wrong = true;
    const deliver = async (intent: FinalToolResultIntent) => {
      intents.push(intent);
      return wrong ? { ...accepted(intent), call_id: "other-call" } : { ...accepted(intent), replayed: true };
    };
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "turn", deliver,
      task => { tasks.push(task); }, new Set(["current_time"]));
    jobs.tool(read).handler({}, context("call-1"));
    const id = jobId(state, "call-1");
    await jobs.reconcile();
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "completed" });
    wrong = false;
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    expect(intents).toHaveLength(2);
    expect(intents[0]).toEqual(intents[1]);
  });
});

it("fences concurrent reconciliation of one terminal call while a core acknowledgement is pending", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    const read: NamedTool = { name: "current_time", description: "read", handler: () => ({ utc: "now" }) };
    let release!: () => void;
    const pending = new Promise<void>(resolve => { release = resolve; });
    let deliveries = 0;
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "turn",
      async intent => { deliveries++; await pending; return accepted(intent, true); },
      task => { tasks.push(task); }, new Set(["current_time"]));
    jobs.tool(read).handler({}, context("call-1"));
    const id = jobId(state, "call-1");
    await jobs.reconcile();
    await Promise.all(tasks);
    const first = jobs.reconcile();
    const second = jobs.reconcile();
    await Promise.resolve();
    expect(deliveries).toBe(1);
    release();
    await Promise.all([first, second]);
    // Even a host hint claiming a started continuation is not an authoritative
    // provider uptake receipt; the concurrent fence still prevents duplicates.
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
  });
});

it("retains a checkpoint without a model continuation and reconciles the same operation after a future wake", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    let executions = 0;
    const read: NamedTool = { name: "current_time", description: "read", handler: () => { executions++; return "now"; } };
    const intents: FinalToolResultIntent[] = [];
    let woke = false;
    const deliver = async (intent: FinalToolResultIntent) => {
      intents.push(intent);
      return { ...accepted(intent, woke), replayed: intents.length > 1 };
    };
    const make = (generation = 0) => new AsyncJobs(state.storage, { current_time: read }, () => "turn", deliver,
      task => { tasks.push(task); }, new Set(["current_time"]), generation);
    const jobs = make();
    jobs.tool(read).handler({}, context("call-1"));
    const id = jobId(state, "call-1");
    await jobs.reconcile();
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    expect(intents).toHaveLength(1);
    await state.storage.deleteAlarm();
    const restored = make();
    // Retention of acknowledged deliveries must not erase an unwoken result.
    state.storage.sql.exec("UPDATE async_jobs SET created_at = ? WHERE id = ?", Date.now() - 8 * 24 * 60 * 60 * 1000, id);
    await restored.reconcile(); // too soon: a stable checkpoint must not spin or falsely claim delivery
    expect(intents).toHaveLength(1);
    expect(restored.status(id)).toMatchObject({ state: "checkpointed" });
    expect(await state.storage.getAlarm()).toBeNull();
    // A new wake-capable generation retries only after the source turn is
    // terminal; an active receipt might otherwise still bind to its next call.
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('turn', 'work', 'completed')");
    woke = true;
    const upgraded = make(1);
    await upgraded.reconcile();
    expect(upgraded.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    expect(intents).toHaveLength(2);
    expect(intents[0]).toEqual(intents[1]);
    expect(executions).toBe(1);
  });
});

it("wraps exec_command as a mutable background tool and checkpoints its single result under the original call", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    const tasks: Promise<unknown>[] = [];
    let executions = 0;
    const shell: NamedTool = { name: "exec_command", description: "shell", handler: input => {
      executions++;
      return { exit_code: 0, output: (input as { cmd: string }).cmd };
    } };
    const intents: FinalToolResultIntent[] = [];
    const jobs = new AsyncJobs(state.storage, { exec_command: shell }, () => "original-turn",
      async intent => { intents.push(intent); return accepted(intent); }, task => { tasks.push(task); });
    expect(jobs.tool(shell).handler({ cmd: "printf safe" }, context("shell-call")))
      .toEqual({ output: UNREAL_RUNNING_OUTPUT });
    const id = jobId(state, "shell-call");
    expect(jobs.status(id)).toMatchObject({ state: "queued" });
    await Promise.all(tasks);
    await jobs.reconcile();
    expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false,
      result: '{"exit_code":0,"output":"printf safe"}' });
    expect(intents).toEqual([{ originalTurn: "original-turn", executionTurn: "turn-1", callId: "shell-call",
      tool: "exec_command", jobId: id, terminalState: "completed",
      output: '{"exit_code":0,"output":"printf safe"}' }]);
    await jobs.reconcile();
    expect(executions).toBe(1);
    expect(intents).toHaveLength(1);
  });
});

it("spills a ninth terminal across the bounded wake without losing or falsely delivering it", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('source', 'work', 'completed')");
    const read: NamedTool = { name: "current_time", description: "read", handler: () => "ok" };
    new AsyncJobs(state.storage, { current_time: read }, () => "source", async () => {}, () => {});
    const ids = Array.from({ length: 9 }, (_, i) => crypto.randomUUID());
    for (let i = 0; i < ids.length; i++) state.storage.sql.exec(`INSERT INTO async_jobs
      (id, invocation, original_turn, execution_turn, call_id, tool, args, state, result,
        terminal_state, created_at) VALUES (?, ?, 'source', 'turn-1', ?, 'current_time', '{}',
        'completed', '"ready"', 'completed', ?)`, ids[i], `turn-1:call-${i}`, `call-${i}`, i);
    const batches: (readonly FinalToolResultIntent[])[] = [];
    let wakeActive = false;
    let firstUptaken = false;
    let secondUptaken = false;
    const makeJobs = () => new AsyncJobs(state.storage, { current_time: read }, () => "source",
      async () => { throw new Error("must not submit separately while idle"); }, () => {},
      new Set(["current_time"]), undefined, async () => ({ state: "pruned_or_unknown" }),
      async intent => (intent.callId === "call-8" ? secondUptaken : firstUptaken)
        ? { state: "confirmed", model_call_index: 3, response_id: "model-3" }
        : { state: "accepted_unbound" },
      async intents => {
        batches.push([...intents]);
        // The native driver rejects a new cohort while the first prompt-less
        // wake is in flight. A mock that accepts 8+1 at once hides this race.
        if (wakeActive) throw new Error("native driver requires an idle boundary");
        wakeActive = true;
        return intents.map(intent => accepted(intent, true));
      });
    let jobs = makeJobs();
    await jobs.reconcile();
    expect(batches.map(batch => batch.length)).toEqual([8]);
    expect(batches[0]!.map(intent => intent.jobId)).toEqual(ids.slice(0, 8));
    for (const id of ids.slice(0, 8)) expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false });
    expect(jobs.status(ids[8]!)).toMatchObject({ state: "completed" });
    // Rehydrate the host's reconciliation object while the native wake is
    // still active; the ninth durable row must not race or vanish on restart.
    jobs = makeJobs();
    await jobs.reconcile();
    expect(batches.map(batch => batch.length)).toEqual([8]);
    expect(jobs.status(ids[8]!)).toMatchObject({ state: "completed" });
    firstUptaken = true;
    wakeActive = false;
    await jobs.reconcile();
    expect(batches.map(batch => batch.length)).toEqual([8, 1]);
    expect(batches.at(-1)![0]!.callId).toBe("call-8");
    for (const id of ids.slice(0, 8)) expect(jobs.status(id)).toMatchObject({ state: "delivered", continuation_started: true });
    expect(jobs.status(ids[8]!)).toMatchObject({ state: "checkpointed", continuation_started: false });
    secondUptaken = true;
    await jobs.reconcile();
    expect(jobs.status(ids[8]!)).toMatchObject({ state: "delivered", continuation_started: true });
  });
});

it("finds an unconfirmed same-source wake beyond the bounded reconciliation page", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('other', 'work', 'completed')");
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('source', 'work', 'completed')");
    const read: NamedTool = { name: "current_time", description: "read", handler: () => "ok" };
    new AsyncJobs(state.storage, { current_time: read }, () => "source", async () => {}, () => {});
    const insert = (id: string, source: string, call: string, stateName: string, at: number) => {
      state.storage.sql.exec(`INSERT INTO async_jobs
        (id, invocation, original_turn, execution_turn, call_id, tool, args, state, result,
          terminal_state, created_at, wake_generation) VALUES (?, ?, ?, 'turn-1', ?,
          'current_time', '{}', ?, '"ready"', 'completed', ?, 0)`,
      id, `turn-1:${call}`, source, call, stateName, at);
    };
    for (let index = 0; index < 24; index++) insert(crypto.randomUUID(), "other", `call-other-${index}`, "completed", index);
    const next = crypto.randomUUID();
    insert(next, "source", "call-source-new", "completed", 24);
    // Twenty-five completed rows fill the page, hiding this older native
    // checkpoint from the per-row status loop. SQL must still fence source.
    insert(crypto.randomUUID(), "source", "call-source-inflight", "checkpointed", 25);
    const batches: string[][] = [];
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "source",
      async () => { throw new Error("unexpected individual output"); }, () => {},
      new Set(["current_time"]), undefined, async () => ({ state: "pruned_or_unknown" }),
      async () => ({ state: "accepted_unbound" }),
      async intents => {
        batches.push(intents.map(intent => intent.callId));
        if (intents.some(intent => intent.callId === "call-source-new"))
          throw new Error("would race in-flight source wake");
        return intents.map(intent => accepted(intent, true));
      });
    await jobs.reconcile();
    expect(batches).toHaveLength(1);
    expect(batches[0]).toEqual(Array.from({ length: 8 }, (_, i) => `call-other-${i}`));
    expect(jobs.status(next)).toMatchObject({ state: "completed" });
  });
});

it("does not stage a batch while another turn is active and fails a mismatched receipt closed", async () => {
  await runInDurableObject(stub(), async (_session, state) => {
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('source', 'work', 'completed')");
    state.storage.sql.exec("INSERT INTO turns (id, input, state) VALUES ('other', 'busy', 'accepted')");
    const read: NamedTool = { name: "current_time", description: "read", handler: () => "ok" };
    new AsyncJobs(state.storage, { current_time: read }, () => "source", async () => {}, () => {});
    const ids = [crypto.randomUUID(), crypto.randomUUID()];
    ids.forEach((id, index) => state.storage.sql.exec(`INSERT INTO async_jobs
      (id, invocation, original_turn, execution_turn, call_id, tool, args, state, result,
        terminal_state, created_at) VALUES (?, ?, 'source', 'turn-1', ?, 'current_time', '{}',
        'completed', '"ready"', 'completed', ?)`, id, `turn-1:call-${index}`, `call-${index}`, index));
    const batches: (readonly FinalToolResultIntent[])[] = [];
    let wrong = true;
    const jobs = new AsyncJobs(state.storage, { current_time: read }, () => "source",
      async () => { throw new Error("no individual fallback"); }, () => {}, new Set(["current_time"]),
      undefined, async () => ({ state: "pruned_or_unknown" }),
      async () => ({ state: "accepted_unbound" }),
      async intents => {
        batches.push([...intents]);
        return intents.map((intent, i) => ({ ...accepted(intent),
          operation_id: wrong && i === 1 ? "forged" : intent.jobId }));
      });
    await jobs.reconcile();
    expect(batches).toHaveLength(0);
    state.storage.sql.exec("UPDATE turns SET state = 'completed' WHERE id = 'other'");
    await jobs.reconcile();
    expect(batches).toHaveLength(1);
    ids.forEach(id => expect(jobs.status(id)).toMatchObject({ state: "completed" }));
    wrong = false;
    await jobs.reconcile();
    expect(batches).toHaveLength(2);
    expect(batches[1]).toEqual(batches[0]);
    ids.forEach(id => expect(jobs.status(id)).toMatchObject({ state: "checkpointed", continuation_started: false }));
  });
});
