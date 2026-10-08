import assert from "node:assert/strict";
import { test } from "node:test";
import { DatabaseSync } from "node:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createCodeRuntime } from "../../nanocodex-tools/runtime/code-runtime.mjs";
import { createManagedCodeEffectJournal } from "../src/managed-recovery-safety.ts";

function storage(db) {
  return { sql: { exec(sql, ...bindings) {
    let rows;
    if (!bindings.length && sql.trim().startsWith("CREATE")) { db.exec(sql); rows = []; }
    else rows = db.prepare(sql).all(...bindings);
    return { toArray: () => rows, one: () => { assert.equal(rows.length, 1); return rows[0]; } };
  } }, transactionSync(fn) {
    db.exec("BEGIN");
    try { const result = fn(); db.exec("COMMIT"); return result; }
    catch (error) { db.exec("ROLLBACK"); throw error; }
  }, async sync() {} };
}
const parse = async value => JSON.parse(await value);
const output = value => typeof value.output === "string" ? value.output : value.output.map(item => item.text ?? "").join("\n");
const cellId = value => output(value).match(/Script running with cell ID (\S+)/)?.[1];
const tick = () => new Promise(resolve => setImmediate(resolve));
function gate() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "code-observations-"));
  const path = join(directory, "observations.sqlite");
  let db = new DatabaseSync(path);
  return { get db() { return db; }, journal() { return createManagedCodeEffectJournal(storage(db)); },
    reopen() { db.close(); db = new DatabaseSync(path); }, close() { db.close(); rmSync(directory, { recursive: true }); } };
}
const identity = () => ({ operationId: "original-op", modelCallIndex: 7 });
const runtime = (journal, tools = {}, extra = {}) => createCodeRuntime(tools, { effectJournal: journal, effectIdentity: identity, ...extra });

test("replayed yield after SQLite reopen recovers completed late receipts, preserves unknown intent, and never dispatches", async () => {
  const f = fixture();
  const secondGate = gate(), secondStarted = gate(), thirdGate = gate(), thirdStarted = gate();
  const counts = { first: 0, second: 0, third: 0 };
  const old = runtime(f.journal(), {
    first: { handler: async () => { counts.first++; return { receipt: "first" }; } },
    second: { handler: async () => { counts.second++; secondStarted.resolve(); await secondGate.promise; return { receipt: "late-second" }; } },
    third: { handler: async () => { counts.third++; thirdStarted.resolve(); await thirdGate.promise; return { receipt: "uncertain-third" }; } },
  });
  let recovered;
  try {
    const active = old.executeCodeObserved('text(await tools.first({})); text(await tools.second({})); await tools.third({ operation_id: "stable-third" });', "owner", "origin");
    await secondStarted.promise;
    old.preempt("owner", "origin");
    // The retained outer tool result can be replayed after restart. No evaluator
    // is reconstructed from this JSON: wait uses only its original public ID.
    const replayedYield = JSON.parse(JSON.stringify(await parse(active)));
    const id = cellId(replayedYield);
    assert.ok(id);
    secondGate.resolve();
    await thirdStarted.promise; // second receipt became durable AFTER the yield
    f.journal(); // fence old owner before delivering uncertain third's response
    await old.reset();
    thirdGate.resolve();
    await tick();
    f.reopen(); // actual disk DB reopened; all runtime objects are replaced
    recovered = runtime(f.journal(), {}, { evaluate: () => { throw new Error("recovery must never evaluate guest source"); } });
    const before = f.db.prepare("SELECT total_changes() AS n").get().n;
    const evidence = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "wait-after-restart"));
    assert.equal(evidence.success, false);
    assert.equal(evidence.cell, undefined);
    assert.deepEqual(evidence.nested_calls, []);
    const text = output(evidence);
    assert.match(text, /CODE_CELL_RECOVERED_EVIDENCE/);
    const details = JSON.parse(text.slice(text.indexOf("\n{") + 1));
    assert.equal(details.previous_observation.cell.running, true);
    assert.deepEqual(details.completed_effect_receipts.map(r => r.call_id), ["origin/code-1", "origin/code-2"]);
    assert.match(JSON.stringify(details.completed_effect_receipts), /late-second/);
    assert.deepEqual(details.pending_effect_call_ids, ["origin/code-3"]);
    assert.equal(details.operation_id, "original-op");
    assert.equal(f.db.prepare("SELECT total_changes() AS n").get().n, before, "recovery is read-only");
    const again = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id, terminate: true }), "owner", "wait-again"));
    assert.deepEqual(again, evidence);
    const foreign = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "stranger", "foreign"));
    assert.match(output(foreign), /CODE_CELL_UNAVAILABLE/);
    assert.doesNotMatch(output(foreign), /late-second|stable-third|original-op/);
    assert.equal(f.db.prepare("SELECT total_changes() AS n").get().n, before);
    assert.deepEqual(counts, { first: 1, second: 1, third: 1 });
    assert.equal(f.db.prepare("SELECT state FROM managed_code_effects WHERE call_id = ?").get("origin/code-3").state, "pending");
  } finally { secondGate.resolve(); thirdGate.resolve(); await old.reset(); await recovered?.reset(); f.close(); }
});

test("terminal observation survives lost acknowledgement and disk reopen without rerunning effects", async () => {
  const f = fixture();
  const ready = gate(), release = gate();
  let calls = 0;
  let loseAck = false;
  const real = f.journal();
  const journal = { ...real, observations: { ...real.observations, async record(...args) {
    await real.observations.record(...args);
    if (loseAck) throw new Error("injected loss after durable observation before acknowledgement");
  } } };
  const old = runtime(journal, { effect: { handler: async () => { calls++; ready.resolve(); await release.promise; return "saved-result"; } } });
  let recovered;
  try {
    const active = old.executeCodeObserved('text(await tools.effect({})); text("final-output");', "owner", "origin");
    await ready.promise; old.preempt("owner", "origin");
    const yielded = await parse(active), id = cellId(yielded);
    release.resolve(); await tick();
    loseAck = true;
    await assert.rejects(old.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "final-wait"), { code: "host_interrupted" });
    const meta = f.db.prepare("SELECT sequence FROM managed_code_public_cells WHERE cell_id = ?").get(id);
    assert.equal(meta.sequence, 3); // running, completion checkpoint, final observer
    await old.reset(); f.reopen();
    recovered = runtime(f.journal());
    const final = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "final-wait"));
    assert.equal(final.success, true);
    assert.equal(final.cell.running, false);
    assert.equal(final.cell.origin_call_id, "origin");
    assert.match(output(final), /final-output/);
    assert.deepEqual(final.nested_calls, []);
    assert.match(output(final), /saved-result/);
    assert.equal(calls, 1);
    assert.deepEqual(await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "final-wait")), final, "same observer reconciles without emitting nested events");
    assert.deepEqual(await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "repeat-final")), final);
  } finally { release.resolve(); await old.reset(); await recovered?.reset(); f.close(); }
});

test("corrupt observation and stale owner fail closed; legacy and foreign IDs cannot recover evidence", async () => {
  const f = fixture();
  const old = f.journal();
  const id = "11111111-1111-4111-8111-111111111111:1";
  const context = { ...identity(), sessionId: "owner", parentCallId: "origin", callId: "origin", source: 'text("ok")', name: "code-cell", input: null };
  try {
    await old.observations.register(context, id);
    const encoded = JSON.stringify({ output: "ok", success: true, cell: { running: false, origin_call_id: "origin" }, nested_calls: [] });
    await old.observations.record("owner", id, encoded);
    const next = f.journal();
    await assert.rejects(old.observations.record("owner", id, encoded), /fenced/);
    assert.equal(await next.observations.recover("stranger", id), null);
    assert.equal(await next.observations.recover("owner", id + "0"), null);
    f.db.prepare("UPDATE managed_code_observation_chunks SET payload = ?").run(encoded.replace("ok", "no"));
    await assert.rejects(next.observations.recover("owner", id), /checksum mismatch/);
  } finally { f.close(); }
});

test("cancellation during durable registration prevents guest execution", async () => {
  const f = fixture(), entered = gate(), release = gate();
  let calls = 0;
  const real = f.journal();
  const r = runtime({ ...real, observations: { ...real.observations, async register(...args) {
    await real.observations.register(...args); entered.resolve(); await release.promise;
  } } }, { effect: { handler: async () => { calls++; return "should not execute"; } } });
  try {
    const active = r.executeCodeObserved('await tools.effect({});', "owner", "origin");
    await entered.promise; r.cancelTurn("owner"); release.resolve();
    const result = await parse(active);
    assert.equal(result.success, false);
    assert.match(output(result), /cancelled during durable admission/);
    assert.equal(calls, 0);
  } finally { release.resolve(); await r.reset(); f.close(); }
});


test("oversized valid observation is retained without failing execution and recovers a bounded final summary", async () => {
  const f = fixture();
  let calls = 0;
  const r = runtime(f.journal(), { large: { handler: async () => { calls++; return "x".repeat(3 * 1024 * 1024); } } });
  let recovered;
  try {
    const result = await parse(r.executeCodeObserved('await tools.large({}); await tools.large({}); text("done-large");', "owner", "large"));
    assert.equal(result.success, true);
    assert.equal(result.nested_calls.length, 2);
    const row = f.db.prepare("SELECT cell_id, bytes FROM managed_code_observations").get();
    assert.ok(row.bytes < 262144);
    await r.reset(); f.reopen(); recovered = runtime(f.journal());
    const final = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: row.cell_id }), "owner", "large-wait"));
    assert.equal(final.success, true);
    assert.equal(final.cell.running, false);
    assert.match(output(final), /done-large/);
    assert.match(output(final), /nested event records omitted/);
    assert.equal(calls, 2);
    assert.ok(output(final).length < 2000);
  } finally { await r.reset(); await recovered?.reset(); f.close(); }
});

test("bounded evidence output honors wait budget while keeping pending counts in its header", async () => {
  const f = fixture(), entered = gate(), release = gate();
  const r = runtime(f.journal(), {
    large: { handler: async () => "x".repeat(100000) },
    pending: { handler: async () => { entered.resolve(); await release.promise; return "unknown"; } },
  });
  let recovered;
  try {
    const active = r.executeCodeObserved('await tools.large({}); await tools.pending({});', "owner", "large-pending");
    await entered.promise; r.preempt("owner", "large-pending");
    const id = cellId(await parse(active));
    f.journal(); await r.reset(); release.resolve(); await tick();
    f.reopen(); recovered = runtime(f.journal());
    const evidence = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id, max_tokens: 10 }), "owner", "small-wait"));
    assert.equal(evidence.success, false);
    assert.match(output(evidence), /1 pending effects have unknown outcomes/);
    assert.ok(output(evidence).length < 2000);
  } finally { release.resolve(); await r.reset(); await recovered?.reset(); f.close(); }
});

test("cancellation aborts identity rendezvous before any durable registration or effect", async () => {
  const f = fixture(), entered = gate();
  let observedSignal, registrations = 0;
  const real = f.journal();
  const r = runtime({ ...real, observations: { ...real.observations, async register(...args) {
    registrations++; return real.observations.register(...args);
  } } }, {}, { effectIdentity: (_session, _parent, _turn, signal) => {
    observedSignal = signal; entered.resolve();
    return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }));
  } });
  try {
    const active = r.executeCodeObserved('text("must not execute");', "owner", "admission");
    await entered.promise; r.cancelTurn("owner");
    const result = await parse(active);
    assert.equal(observedSignal.aborted, true);
    assert.equal(result.success, false);
    assert.equal(registrations, 0);
  } finally { await r.reset(); f.close(); }
});

test("retention keeps only the latest observation per cell and bounds total envelopes", async () => {
  const f = fixture(), j = f.journal();
  const context = { ...identity(), sessionId: "owner", parentCallId: "origin", callId: "origin", source: 'text("ok")', name: "code-cell", input: null };
  const encoded = JSON.stringify({ output: "ok", success: true, cell: { running: false, origin_call_id: "origin" }, nested_calls: [] });
  try {
    for (let i = 1; i <= 130; i++) {
      const id = "11111111-1111-4111-8111-111111111111:" + i;
      await j.observations.register(context, id);
      await j.observations.record("owner", id, encoded);
      await j.observations.record("owner", id, encoded);
    }
    assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM managed_code_observations").get().n, 128);
    assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM managed_code_observation_chunks").get().n, 128);
    const evicted = JSON.parse(await j.observations.recover("owner", "11111111-1111-4111-8111-111111111111:1"));
    assert.equal(evicted.success, false);
    assert.equal(evicted.cell, undefined);
    assert.match(output(evicted), /"observation_retention":"evicted"/);
    // A live owner can record a newer observation after earlier payload eviction.
    await j.observations.record("owner", "11111111-1111-4111-8111-111111111111:1", encoded);
    assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM managed_code_observation_evictions WHERE cell_id = ?").get("11111111-1111-4111-8111-111111111111:1").n, 0);
    assert.equal(JSON.parse(await j.observations.recover("owner", "11111111-1111-4111-8111-111111111111:1")).success, true);
    const final = JSON.parse(await j.observations.recover("owner", "11111111-1111-4111-8111-111111111111:130"));
    assert.equal(final.success, true);
    assert.deepEqual(final.nested_calls, []);
  } finally { f.close(); }
});

test("Claude production host consumes restarted terminal evidence without new nested events", async () => {
  const { createClaudeHost } = await import("../../nanocodex/runtime/claude-host.mjs");
  const f = fixture();
  const j = f.journal();
  const id = "11111111-1111-4111-8111-111111111111:1";
  const context = { ...identity(), sessionId: "owner", parentCallId: "origin", callId: "origin", source: 'text("ok")', name: "code-cell", input: null };
  let host;
  try {
    await j.observations.register(context, id);
    await j.observations.record("owner", id, JSON.stringify({
      output: "terminal-proof", success: true, cell: { running: false, origin_call_id: "origin" },
      nested_calls: [{ call_id: "origin/code-1", name: "effect", success: true, output: "historical-receipt" }],
    }));
    f.reopen();
    host = createClaudeHost({ auth: { headers: async () => { throw new Error("no network auth during recovery"); } },
      toolMode: "code-only", codeEffectJournal: f.journal(),
      codeEvaluator: () => { throw new Error("no guest evaluation during recovery"); } });
    for (const callId of ["same-observer", "same-observer", "new-observer"]) {
      const result = JSON.parse(await host.executeClaudeTool("wait", JSON.stringify({ cell_id: id }), "owner", callId, "test", "new-turn"));
      assert.equal(result.isError, false);
      assert.match(JSON.stringify(result.content), /terminal-proof/);
      assert.match(JSON.stringify(result.content), /historical-receipt/);
      assert.deepEqual(result.metadata._nanocodex_code.calls, []);
      assert.equal(result.metadata._nanocodex_code.origin_call_id, "origin");
    }
  } finally { await host?.dispose(); f.close(); }
});

test("retention enforces aggregate byte bound as well as count", async () => {
  const f = fixture(), j = f.journal();
  const context = { ...identity(), sessionId: "owner", parentCallId: "origin", callId: "origin", source: 'text("ok")', name: "code-cell", input: null };
  const encoded = JSON.stringify({ output: "x".repeat(7 * 1024 * 1024), success: true,
    cell: { running: false, origin_call_id: "origin" }, nested_calls: [] });
  try {
    for (let i = 1; i <= 5; i++) {
      const id = "11111111-1111-4111-8111-111111111111:" + i;
      await j.observations.register(context, id);
      await j.observations.record("owner", id, encoded);
    }
    const retained = f.db.prepare("SELECT COUNT(*) AS n, SUM(bytes) AS bytes FROM managed_code_observations").get();
    assert.equal(retained.n, 4);
    assert.ok(retained.bytes <= 32 * 1024 * 1024);
    const chunks = f.db.prepare("SELECT SUM(length(CAST(payload AS BLOB))) AS bytes FROM managed_code_observation_chunks").get();
    assert.equal(chunks.bytes, retained.bytes);
    const evicted = JSON.parse(await j.observations.recover("owner", "11111111-1111-4111-8111-111111111111:1"));
    assert.equal(evicted.success, false);
    assert.equal(evicted.cell, undefined);
    assert.match(output(evicted), /"observation_retention":"evicted"/);
  } finally { f.close(); }
});

test("explicit eviction after SQLite restart preserves original completed receipts and unknown intents read-only", async () => {
  const f = fixture(), j = f.journal(), entered = gate(), release = gate();
  let calls = 0, recovered;
  const r = runtime(j, {
    done: { handler: async () => { calls++; return "original-completed-receipt"; } },
    pending: { handler: async () => { calls++; entered.resolve(); await release.promise; return "late"; } },
  });
  try {
    const active = r.executeCodeObserved('await tools.done({}); await tools.pending({ operation_id: "stable-pending" });', "owner", "origin");
    await entered.promise; r.preempt("owner", "origin");
    const id = cellId(await parse(active));
    const context = { ...identity(), sessionId: "owner", parentCallId: "filler", callId: "filler", source: 'text("ok")', name: "code-cell", input: null };
    const encoded = JSON.stringify({ output: "filler", success: true, cell: { running: false, origin_call_id: "filler" }, nested_calls: [] });
    for (let i = 1; i <= 128; i++) {
      const fillerId = "11111111-1111-4111-8111-111111111111:" + i;
      await j.observations.register(context, fillerId);
      await j.observations.record("owner", fillerId, encoded);
    }
    assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM managed_code_observations WHERE cell_id = ?").get(id).n, 0);
    assert.equal(f.db.prepare("SELECT sequence FROM managed_code_observation_evictions WHERE cell_id = ?").get(id).sequence, 1);
    f.journal(); await r.reset(); release.resolve(); await tick();
    f.reopen();
    recovered = runtime(f.journal(), {}, { evaluate: () => { throw new Error("must not evaluate"); } });
    const before = f.db.prepare("SELECT total_changes() AS n").get().n;
    const evidence = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "after-eviction"));
    const text = output(evidence);
    const details = JSON.parse(text.slice(text.indexOf("\n{") + 1));
    assert.equal(details.observation_retention, "evicted");
    assert.equal(details.previous_observation, null);
    assert.deepEqual(details.pending_effect_call_ids, ["origin/code-2"]);
    assert.equal(details.completed_effect_receipts[0].call_id, "origin/code-1");
    assert.match(JSON.stringify(details.completed_effect_receipts), /original-completed-receipt/);
    assert.equal(evidence.success, false);
    assert.equal(evidence.cell, undefined);
    assert.deepEqual(evidence.nested_calls, []);
    assert.deepEqual(await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id, terminate: true }), "owner", "new-wait")), evidence);
    const foreign = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "stranger", "foreign"));
    assert.doesNotMatch(output(foreign), /original-completed-receipt|stable-pending/);
    assert.equal(f.db.prepare("SELECT total_changes() AS n").get().n, before);
    assert.equal(calls, 2);
  } finally { release.resolve(); await r.reset(); await recovered?.reset(); f.close(); }
});

test("unmarked missing metadata and mismatched eviction sequence fail closed", async () => {
  const f = fixture();
  let j = f.journal();
  const id = "11111111-1111-4111-8111-111111111111:1";
  const context = { ...identity(), sessionId: "owner", parentCallId: "origin", callId: "origin", source: 'text("ok")', name: "code-cell", input: null };
  try {
    await j.observations.register(context, id);
    await j.observations.record("owner", id, JSON.stringify({ output: "ok", success: true, cell: { running: false, origin_call_id: "origin" }, nested_calls: [] }));
    f.db.prepare("DELETE FROM managed_code_observations WHERE cell_id = ?").run(id);
    // Upgrade from a ledger predating eviction markers must not guess eviction.
    f.db.exec("DROP TABLE managed_code_observation_evictions");
    f.reopen(); j = f.journal();
    await assert.rejects(j.observations.recover("owner", id), /metadata is missing/);
    f.db.prepare("INSERT INTO managed_code_observation_evictions VALUES (?, ?)").run(id, 2);
    await assert.rejects(j.observations.recover("owner", id), /eviction identity mismatch/);
  } finally { f.close(); }
});


test("completion wins over a delayed running observation acknowledgement before restart", async () => {
  const f = fixture(), entered = gate(), release = gate(), recording = gate(), ack = gate(), terminal = gate();
  let calls = 0, recovered;
  const real = f.journal();
  const r = runtime({ ...real, observations: { ...real.observations, async record(...args) {
    const running = JSON.parse(args[2]).cell.running;
    await real.observations.record(...args);
    if (running) { recording.resolve(); await ack.promise; }
    else terminal.resolve();
  } } }, { effect: { handler: async () => { calls++; entered.resolve(); await release.promise; return "saved-once"; } } });
  try {
    const active = r.executeCodeObserved('text(await tools.effect({})); text("background-final");', "owner", "origin");
    await entered.promise; r.preempt("owner", "origin");
    await recording.promise;
    release.resolve(); await tick();
    ack.resolve();
    const id = cellId(await parse(active));
    await terminal.promise; await tick();
    // No wait consumed completion. The last durable observation must nevertheless
    // be terminal, even though the foreground yield acknowledged after completion.
    await r.reset(); f.reopen(); recovered = runtime(f.journal());
    const final = await parse(recovered.waitCodeObserved(JSON.stringify({ cell_id: id }), "owner", "after-restart"));
    assert.equal(final.success, true);
    assert.equal(final.cell.running, false);
    assert.match(output(final), /background-final/);
    assert.match(output(final), /saved-once/);
    assert.deepEqual(final.nested_calls, []);
    assert.equal(calls, 1);
  } finally { release.resolve(); ack.resolve(); await r.reset(); await recovered?.reset(); f.close(); }
});


test("runtime reset does not checkpoint cancellation as settled execution", async () => {
  const f = fixture(), entered = gate(), release = gate();
  let calls = 0, recovered;
  const r = runtime(f.journal(), { effect: { handler: async () => {
    calls++; entered.resolve(); await release.promise; return "late";
  } } });
  try {
    const active = r.executeCodeObserved('await tools.effect({operation_id:"original"});', "owner", "origin");
    await entered.promise; r.preempt("owner", "origin");
    const id = cellId(await parse(active));
    await r.reset(); await tick();
    f.reopen(); recovered = runtime(f.journal());
    const evidence = await parse(recovered.waitCodeObserved(JSON.stringify({cell_id:id}), "owner", "after-reset"));
    assert.equal(evidence.success, false);
    assert.equal(evidence.cell, undefined);
    assert.match(output(evidence), /1 pending effects have unknown outcomes/);
    assert.deepEqual(evidence.nested_calls, []);
    assert.equal(calls, 1);
  } finally { release.resolve(); await tick(); await r.reset(); await recovered?.reset(); f.close(); }
});
