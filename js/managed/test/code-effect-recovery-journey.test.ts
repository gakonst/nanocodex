import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import type { AgentEvent, CodeEvaluatorEnvironment } from "nanocodex";
import { createManagedCodeEffectJournal, ManagedRecoverySafety } from "../src/managed-recovery-safety";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import { durabilityRevision } from "nanocodex/durability";
import { managedCodeEvaluator } from "../src/code-evaluator";
// Public JavaScript runtime subpath does not publish a declaration file.
// @ts-expect-error Integration exercises the shipped runtime and real QuickJS.
import { createCodeRuntime } from "nanocodex-tools/runtime/code-runtime";

// Exact pending head from real Rust/WASM's no-journal owned SDK abrupt-restart
// negative control in code-recovery-wasm.test.mjs (revision6, format4).
// This seeds only metadata: shipped QuickJS + Workers SQLite admission is real;
// this journey does not pretend to reconstruct the Rust execution owner.
const LEGACY_HEAD = "{\"nanocodex_durable_state\":{\"format\":4,\"operations\":{\"original\":{\"continuation\":\"3c5c10801f261a05de2f1c96ed2de92bd2c585c2e0c60fb9d34a49aa0ef5f659\",\"retired_model_calls\":0,\"retired_steers\":0,\"input\":\"881eadb99c9a3c4e1d58bfd4e1569080ff079eb944a06be1bbf6a79151b1a998\",\"status\":\"pending\",\"steps\":{\"model-1\":{\"kind\":\"model_call\",\"input\":\"74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b\",\"status\":{\"completed\":\"9f977b7a24a53e7d2ea5bad27008a0b5f63ee3d812cf07a280da3f3f63b374ed\"},\"attempts\":1},\"tool-1-owned-cell\":{\"kind\":\"tool_call\",\"input\":\"3c70640a0a05d2050b160eb3ecda5a76acf05676101e07a9e7300689a20b2b3d\",\"status\":\"effect_pending\",\"attempts\":1}},\"accepted_order\":1}},\"latest_checkpoint\":null}}";
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const effectScope = (operationId = "original", modelCallIndex = 1) => () => ({ operationId, modelCallIndex });
async function seedLegacyHead(storage: DurableObjectStorage) {
  const store = createCloudflareDurabilityStore(storage);
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_agent (
    singleton INTEGER PRIMARY KEY, session_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_durability (
    singleton INTEGER PRIMARY KEY, state_id TEXT NOT NULL)`);
  storage.sql.exec("INSERT OR REPLACE INTO nanocodex_cloudflare_agent VALUES (1, 'fixture-session')");
  storage.sql.exec("INSERT OR REPLACE INTO nanocodex_cloudflare_durability VALUES (1, 'fixture-root')");
  await store.importState("fixture-root", { revision: durabilityRevision("6"), payload: LEGACY_HEAD });
  // The pristine test DO was constructed before we seeded old owner metadata.
  // Erase only its marker to model FIRST rollout onto this preexisting head.
  storage.sql.exec("DELETE FROM managed_code_effect_migration");
}

it("reuses completed nested results and fences an uncertain effect across managed owner loss", async () => {
  const namespace = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const counts = { one: 0, two: 0, write: 0 };
    let release!: () => void;
    let entered!: () => void;
    const started = new Promise<void>(resolve => { entered = resolve; });
    const pending = new Promise<void>(resolve => { release = resolve; });
    const tools = {
      one: { handler: async () => { counts.one++; return { value: "first durable result" }; } },
      two: { handler: async () => { counts.two++; return { value: "second durable result" }; } },
      write: { handler: async () => { counts.write++; entered(); await pending; return "merchant accepted"; } },
    };
    const source = 'text((await tools.one({})).value); text((await tools.two({})).value); await tools.write({ operation_id: "fixture-original-operation" });';
    const original = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage) });
    const old = original.executeCode(source, "fixture-session", "fixture-cell").then(() => "completed", () => "interrupted");
    await started;
    expect(counts).toEqual({ one: 1, two: 1, write: 1 });
    expect(ctx.storage.sql.exec("SELECT state FROM managed_code_effects ORDER BY call_id").toArray())
      .toEqual([{ state: "completed" }, { state: "completed" }, { state: "pending" }]);
    // Fence the old owner before its response arrives. The effects already
    // happened; a new QuickJS session may replay only acknowledged receipts.
    const recoveredJournal = createManagedCodeEffectJournal(ctx.storage);
    release();
    expect(await old).toBe("interrupted");
    const recovered = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: recoveredJournal });
    const result = JSON.parse(await recovered.executeCode(source, "fixture-session", "fixture-cell"));
    expect(result.success).toBe(false);
    expect(result.output).toContain("outcome unknown");
    expect(counts).toEqual({ one: 1, two: 1, write: 1 });
    expect(result.nested_calls.slice(0, 2).map((call: { structured_result: unknown }) => call.structured_result))
      .toEqual([{ value: "first durable result" }, { value: "second durable result" }]);
    expect(ctx.storage.sql.exec("SELECT state FROM managed_code_effects WHERE call_id='fixture-cell/code-3'").one()).toEqual({ state: "pending" });
    const next = JSON.parse(await recovered.executeCode('text((await tools.one({})).value);', "fixture-session", "fixture-next-cell"));
    expect(next.success).toBe(true);
    expect(counts.one).toBe(2);
    console.log("CODE_EFFECT_RECOVERY_JOURNEY", JSON.stringify({ counts, result, next }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("fences the exact legacy parent after >512 noise/archive deletion and retains its fence across owners", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    // Old global2 is different from the new cell-local1. This old telemetry is
    // metadata-only because running a removed old host is not this test's remit.
    ctx.storage.sql.exec("INSERT INTO managed_events (turn_id,message_json,created_at) VALUES (?,?,?)", "legacy",
      JSON.stringify({ type: "event", event: { type: "tool.call", payload: { call_id: "owned-cell/code-2" } } }), Date.now());
    for (let index = 0; index < 600; index++) ctx.storage.sql.exec(
      "INSERT INTO managed_events (turn_id,message_json,created_at) VALUES (?,?,?)", "noise",
      JSON.stringify({ type: "event", event: { type: "run.started", payload: { index } } }), Date.now());
    ctx.storage.sql.exec("DELETE FROM managed_events WHERE turn_id='legacy'");
    expect(ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM managed_events").one()).toEqual({ count: 600 });
    const sdkOwners = ctx.storage.sql.exec("SELECT * FROM nanocodex_durable_owners").toArray();
    let writes = 0;
    const tools = { write: { handler: async () => { writes++; return "accepted"; } } };
    const make = (operationId = "original", modelCallIndex = 1) => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope(operationId, modelCallIndex) });
    const source = 'await Promise.all([tools.write({ operation_id: "legacy-original-operation" }), tools.write({ operation_id: "other" })]);';
    const legacy = JSON.parse(await make().executeCode(source, "fixture-session", "owned-cell"));
    expect(legacy.success).toBe(false);
    expect(legacy.output).toContain("outcome unknown");
    expect(legacy.nested_calls).toEqual([]);
    expect(writes).toBe(0);
    expect(ctx.storage.sql.exec("SELECT * FROM managed_code_effect_legacy_parents").toArray())
      .toEqual([{ session_id: "fixture-session", parent_call_id: JSON.stringify(["original", 1, "owned-cell"]), state_id: "fixture-root", step_key: "tool-1-owned-cell", scope_version: 2 }]);
    // First unknown journal intent must not erase the parent fence on restart.
    ctx.storage.sql.exec("DELETE FROM nanocodex_durable_states");
    const repeated = JSON.parse(await make().executeCode(source, "fixture-session", "owned-cell"));
    expect(repeated.output).toContain("outcome unknown");
    expect(ctx.storage.sql.exec("SELECT state FROM managed_code_effects WHERE call_id='owned-cell/code-2'").toArray()).toEqual([]);
    const nextOperation = JSON.parse(await make("next-operation").executeCode(source, "fixture-session", "owned-cell"));
    expect(nextOperation.success).toBe(true);
    const nextIndex = JSON.parse(await make("original", 2).executeCode(source, "fixture-session", "owned-cell"));
    expect(nextIndex.success).toBe(true);
    const next = JSON.parse(await make().executeCode(source, "fixture-session", "new-cell"));
    expect(next.success).toBe(true);
    const newSession = JSON.parse(await make().executeCode(source, "new-session", "owned-cell"));
    expect(newSession.success).toBe(true);
    expect(writes).toBe(8);
    expect(ctx.storage.sql.exec("SELECT * FROM nanocodex_durable_owners").toArray()).toEqual(sdkOwners);
    console.log("CODE_EFFECT_ARCHIVE_FREE_UPGRADE_JOURNEY", JSON.stringify({ writes, legacy, repeated, nextOperation, nextIndex, next, newSession }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("fences a pending cell after owner loss even when its first nested ordinal completed", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let reads = 0; let writes = 0;
    let entered!: () => void; let release!: () => void;
    const paused = new Promise<void>(resolve => { entered = resolve; });
    const lostOwner = new Promise<void>(resolve => { release = resolve; });
    const journal = createManagedCodeEffectJournal(ctx.storage);
    const tools = { one: { handler: async () => { reads++; return "completed"; } }, write: { handler: async () => { writes++; return "new ordinal"; } } };
    const source = 'text(await tools.one({})); text(await tools.write({}));';
    const original = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectIdentity: effectScope(), effectJournal: {
      ...journal, async begin(context: Parameters<typeof journal.begin>[0]) {
        // Unavoidable owner-loss injection immediately before the next intent,
        // not a fake tool result: the first receipt is real and durably stored.
        if (context.callId.endsWith("/code-2")) { entered(); await lostOwner; throw new Error("fixture owner lost before next intent"); }
        return journal.begin(context);
      },
    } });
    const old = original.executeCode(source, "fixture-session", "owned-cell").catch(() => "interrupted");
    await paused;
    expect({ reads, writes }).toEqual({ reads: 1, writes: 0 });
    await seedLegacyHead(ctx.storage);
    const replacement = createManagedCodeEffectJournal(ctx.storage);
    release(); await old;
    expect(ctx.storage.sql.exec("SELECT * FROM managed_code_effect_legacy_parents").toArray()).toEqual([]);
    const recovered = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: replacement, effectIdentity: effectScope() });
    const result = JSON.parse(await recovered.executeCode(source, "fixture-session", "owned-cell"));
    expect(result.success).toBe(false);
    expect(result.output).toContain("outcome unknown");
    expect({ reads, writes }).toEqual({ reads: 1, writes: 0 });
    console.log("CODE_EFFECT_JOURNALLED_PARENT_UPGRADE_JOURNEY", JSON.stringify({ reads, writes, result }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("fences orphan child-head pending parents and old child sessions but permits fresh identities", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    await createCloudflareDurabilityStore(ctx.storage).importState("orphan-child-head", { revision: durabilityRevision("6"), payload: LEGACY_HEAD });
    ctx.storage.sql.exec(`CREATE TABLE nanocodex_cloudflare_subagents (session_id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL, descriptor_json TEXT NOT NULL, host_context_ref TEXT);
      INSERT INTO nanocodex_cloudflare_subagents VALUES ('retained-child', '1', '{}', NULL);
      CREATE TABLE nanocodex_cloudflare_subagent_checkpoints (chunk_index INTEGER PRIMARY KEY, payload TEXT NOT NULL)
    `);
    // Minimal legacy checkpoint identity metadata, from the removed SDK schema;
    // no guessed runtime state or reconstructed child outputs are consumed.
    ctx.storage.sql.exec("INSERT INTO nanocodex_cloudflare_subagent_checkpoints VALUES (0, ?)",
      JSON.stringify({ root_session_id: "fixture-session", children: [{ descriptor: { session_id: "checkpoint-child" } }] }));
    let writes = 0;
    const tools = { write: { handler: async () => { writes++; return "accepted"; } } };
    const make = (operationId = "original", modelCallIndex = 1) => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope(operationId, modelCallIndex) });
    const source = 'await tools.write({ operation_id: "child-original-operation" });';
    const orphan = JSON.parse(await make().executeCode(source, "unlinked-child", "owned-cell"));
    const retained = JSON.parse(await make().executeCode(source, "retained-child", "unknown-parent"));
    const checkpointChild = JSON.parse(await make().executeCode(source, "checkpoint-child", "checkpoint-parent"));
    expect(orphan.output).toContain("outcome unknown");
    expect(retained.output).toContain("outcome unknown");
    expect(checkpointChild.output).toContain("outcome unknown");
    expect(writes).toBe(0);
    ctx.storage.sql.exec("DROP TABLE nanocodex_cloudflare_subagents; DROP TABLE nanocodex_cloudflare_subagent_checkpoints; DELETE FROM nanocodex_durable_states");
    const persisted = JSON.parse(await make().executeCode(source, "retained-child", "another-parent"));
    expect(persisted.output).toContain("outcome unknown");
    const fresh = JSON.parse(await make().executeCode(source, "fresh-child", "new-parent"));
    expect(fresh.success).toBe(true);
    expect(writes).toBe(1);
    console.log("CODE_EFFECT_CHILD_UPGRADE_JOURNEY", JSON.stringify({ writes, orphan, retained, checkpointChild, persisted, fresh }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it.each(["corrupt", "unsupported", "oversized"])("fails closed on a %s existing head without hydrating unbounded state", async kind => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    const payload = kind === "oversized" ? "x".repeat(1024 * 1024 + 1)
      : kind === "unsupported" ? LEGACY_HEAD.replace('"format":4', '"format":5') : "{corrupt";
    ctx.storage.sql.exec("UPDATE nanocodex_durable_states SET payload=?", payload);
    let writes = 0; let headReads = 0;
    const storage = {
      sql: { exec(sql: string, ...args: Array<string | number | null>) {
        if (sql.startsWith("SELECT revision, payload FROM nanocodex_durable_states")) headReads++;
        return ctx.storage.sql.exec(sql, ...args);
      } },
      transactionSync: <T>(callback: () => T) => ctx.storage.transactionSync(callback),
      sync: () => ctx.storage.sync(),
    } as DurableObjectStorage;
    const tools = { write: { handler: async () => { writes++; return "must not dispatch"; } } };
    const make = () => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(storage), effectIdentity: effectScope() });
    const source = 'await tools.write({});';
    const result = JSON.parse(await make().executeCode(source, "fixture-session", "owned-cell"));
    expect(result.output).toContain("outcome unknown");
    expect(writes).toBe(0);
    expect(headReads).toBe(kind === "oversized" ? 0 : 1);
    const readsAfterSnapshot = headReads;
    const next = JSON.parse(await make().executeCode(source, "fixture-session", "different-cell"));
    expect(next.output).toContain("outcome unknown");
    expect(headReads).toBe(readsAfterSnapshot);
    expect(writes).toBe(0);
    console.log("CODE_EFFECT_UNREADABLE_HEAD_JOURNEY", JSON.stringify({ kind, writes, headReads, result, next }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("treats provider call ID reuse as fresh across original operations/model ordinals but replays original scope after projected turn changes", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let writes = 0;
    const tools = { write: { handler: async (input: { value: string }) => { writes++; return input.value; } } };
    const make = (operationId: string, modelCallIndex: number) => createCodeRuntime(tools, {
      evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope(operationId, modelCallIndex),
    });
    const firstSource = 'text(await tools.write({ value: "first receipt" }));';
    const first = JSON.parse(await make("original", 1).executeCode(firstSource, "fixture-session", "call0", "unknown", undefined, undefined, "projection-a"));
    expect(first.success).toBe(true);
    expect(writes).toBe(1);
    const restarted = JSON.parse(await make("original", 1).executeCode(firstSource, "fixture-session", "call0", "unknown", undefined, undefined, "projection-b"));
    expect(restarted.nested_calls[0].structured_result).toEqual(first.nested_calls[0].structured_result);
    expect(writes).toBe(1);
    const other = JSON.parse(await make("other-operation", 1).executeCode('text(await tools.write({ value: "new operation" }));', "fixture-session", "call0"));
    expect(other.success).toBe(true);
    expect(JSON.stringify(other.output)).toContain("new operation");
    const nextModel = JSON.parse(await make("original", 2).executeCode('text(await tools.write({ value: "new model ordinal" }));', "fixture-session", "call0"));
    expect(nextModel.success).toBe(true);
    expect(JSON.stringify(nextModel.output)).toContain("new model ordinal");
    expect(writes).toBe(3);
    const originalAgain = JSON.parse(await make("original", 1).executeCode(firstSource, "fixture-session", "call0"));
    expect(originalAgain.nested_calls[0].structured_result).toEqual(first.nested_calls[0].structured_result);
    expect(writes).toBe(3);
    const missingScope = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage) });
    const undecidable = JSON.parse(await missingScope.executeCode(firstSource, "fixture-session", "call0"));
    expect(undecidable.output).toContain("outcome unknown");
    expect(writes).toBe(3);
    const partialScope = createCodeRuntime(tools, {
      evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: () => ({ operationId: "original" }),
    });
    expect(JSON.parse(await partialScope.executeCode(firstSource, "fixture-session", "call0")).output).toContain("outcome unknown");
    expect(writes).toBe(3);
    const direct = JSON.parse(await make("original", 3).executeTool("write", JSON.stringify({ value: "direct receipt" }), "fixture-session", "call0", "unknown", "projection-c"));
    const directReplay = JSON.parse(await make("original", 3).executeTool("write", JSON.stringify({ value: "direct receipt" }), "fixture-session", "call0", "unknown", "projection-d"));
    expect(directReplay).toEqual(direct);
    expect(writes).toBe(4);
    console.log("SCOPED_PROVIDER_ID_REUSE_JOURNEY", JSON.stringify({ writes, first, restarted, other, nextModel, originalAgain, undecidable, direct, directReplay }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("does not let a different operation's journalled same parent exempt an exact legacy pending scope", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let writes = 0;
    const tools = { write: { handler: async () => { writes++; return "accepted"; } } };
    const make = (operationId: string) => createCodeRuntime(tools, {
      evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope(operationId),
    });
    const source = 'text(await tools.write({}));';
    const unrelated = JSON.parse(await make("unrelated").executeCode(source, "fixture-session", "owned-cell"));
    expect(unrelated.success).toBe(true);
    await seedLegacyHead(ctx.storage);
    const original = JSON.parse(await make("original").executeCode(source, "fixture-session", "owned-cell"));
    expect(original.output).toContain("outcome unknown");
    expect(writes).toBe(1);
    const unrelatedReplay = JSON.parse(await make("unrelated").executeCode(source, "fixture-session", "owned-cell"));
    expect(unrelatedReplay.success).toBe(true);
    expect(writes).toBe(1);
    const fresh = JSON.parse(await make("new-operation").executeCode(source, "fixture-session", "owned-cell"));
    expect(fresh.success).toBe(true);
    expect(writes).toBe(2);
    console.log("SCOPED_LEGACY_EXCLUSION_JOURNEY", JSON.stringify({ writes, unrelated, original, unrelatedReplay, fresh }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("keeps v1 live-schema receipts and parent fences unknown rather than assigning guessed operation scopes", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let writes = 0;
    const tools = { write: { handler: async () => { writes++; return "old real receipt"; } } };
    const source = 'text(await tools.write({}));';
    const old = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage) });
    expect(JSON.parse(await old.executeCode(source, "fixture-session", "old-cell")).success).toBe(true);
    // Unavoidable old-host schema boundary fixture: retain the actual QuickJS
    // receipt and intent, but remove the metadata that the old v1 host lacked.
    ctx.storage.sql.exec(`CREATE TABLE fixture_v1_effects (
      effect_key TEXT PRIMARY KEY, session_id TEXT NOT NULL, turn_id TEXT,
      parent_call_id TEXT NOT NULL, call_id TEXT NOT NULL, name TEXT NOT NULL,
      input_hash TEXT NOT NULL, generation TEXT NOT NULL, state TEXT NOT NULL,
      receipt_chunks INTEGER, created_at INTEGER NOT NULL, completed_at INTEGER);
      INSERT INTO fixture_v1_effects SELECT json_array(session_id,parent_call_id,call_id), session_id,turn_id,parent_call_id,call_id,name,input_hash,generation,state,receipt_chunks,created_at,completed_at FROM managed_code_effects;
      UPDATE managed_code_effect_receipt_chunks SET effect_key = json_array(
        json_extract(effect_key, '$[0]'), json_extract(effect_key, '$[3]'), json_extract(effect_key, '$[4]'));
      DROP TABLE managed_code_effects;
      ALTER TABLE fixture_v1_effects RENAME TO managed_code_effects;
      CREATE TABLE fixture_v1_parents (
        session_id TEXT NOT NULL, parent_call_id TEXT NOT NULL, state_id TEXT NOT NULL,
        step_key TEXT NOT NULL, PRIMARY KEY(session_id,parent_call_id));
      INSERT INTO fixture_v1_parents VALUES ('fixture-session','old-parent','retired-head','tool-1-old-parent');
      DROP TABLE managed_code_effect_legacy_parents;
      ALTER TABLE fixture_v1_parents RENAME TO managed_code_effect_legacy_parents;
      UPDATE managed_code_effect_migration SET version = 1`);
    const make = () => createCodeRuntime(tools, {
      evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope(),
    });
    const receiptUnknown = JSON.parse(await make().executeCode(source, "fixture-session", "old-cell"));
    const parentUnknown = JSON.parse(await make().executeCode(source, "fixture-session", "old-parent"));
    expect(receiptUnknown.output).toContain("outcome unknown");
    expect(parentUnknown.output).toContain("outcome unknown");
    expect(writes).toBe(1);
    const fresh = JSON.parse(await make().executeCode(source, "fixture-session", "fresh-parent"));
    expect(fresh.success).toBe(true);
    expect(writes).toBe(2);
    expect(ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM managed_code_effects WHERE scope_version=1").one()).toEqual({ count: 1 });
    console.log("V1_SCHEMA_FAIL_CLOSED_JOURNEY", JSON.stringify({ writes, receiptUnknown, parentUnknown, fresh }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("replenishes recovery only for a new model ordinal's real receipt, not a replayed provider ID result lacking an ordinal", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const safety = new ManagedRecoverySafety(ctx.storage);
    let writes = 0; let index = 1; let seq = 0;
    const tools = { write: { handler: async () => { writes++; return "accepted"; } } };
    const source = 'text(await tools.write({}));';
    const make = () => createCodeRuntime(tools, {
      evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: () => ({ operationId: "original", modelCallIndex: index }),
    });
    // Real guest/tool callbacks supply call IDs and completed receipts. The
    // narrow adapter represents Rust's documented event shape: start carries
    // the stable model index; result omits it. Full Rust-owner paths are covered
    // by the managed process hard-loss journey, not replaced by this adapter.
    const observer = (update: { type: string; call_id?: string; call?: { call_id: string } }) => {
      if (update.type === "nested_call_started") safety.progress("original", {
        protocol_version: 1, request_id: "projected-runtime-id", seq: ++seq, type: "tool.call",
        payload: { call_id: update.call_id, model_call_index: index },
      } as unknown as AgentEvent);
      if (update.type === "nested_call_completed") safety.progress("original", {
        protocol_version: 1, request_id: "different-projected-runtime-id", seq: ++seq, type: "tool.result",
        payload: { call_id: update.call?.call_id },
      } as unknown as AgentEvent);
    };
    expect(safety.begin("original")).toBe(false);
    expect(JSON.parse(await make().executeCode(source, "fixture-session", "call0", observer)).success).toBe(true);
    expect(writes).toBe(1);
    expect(safety.begin("original")).toBe(false);
    const replay = JSON.parse(await make().executeCode(source, "fixture-session", "call0", observer));
    expect(replay.success).toBe(true);
    expect(writes).toBe(1);
    expect(ctx.storage.sql.exec("SELECT abrupt_attempts FROM managed_recovery_safety WHERE turn_id='original'").one()).toEqual({ abrupt_attempts: 1 });
    expect(safety.begin("original")).toBe(false);
    index = 2;
    const nextIndex = JSON.parse(await make().executeCode(source, "fixture-session", "call0", observer));
    expect(nextIndex.success).toBe(true);
    expect(writes).toBe(2);
    const admissions = [safety.begin("original"), safety.begin("original"), safety.begin("original")];
    expect(admissions).toEqual([false, false, false]);
    expect(ctx.storage.sql.exec("SELECT * FROM managed_recovery_call_indices").toArray()).toEqual([]);
    console.log("MODEL_ORDINAL_RECOVERY_PROGRESS_JOURNEY", JSON.stringify({ writes, replay, nextIndex, admissions }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("fences an unfinished cell after owner loss and preserves its earlier committed store", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let calls = 0;
    const tools = { read: { handler: async (input: unknown) => { calls++; return input; } } };
    const make = (journal = createManagedCodeEffectJournal(ctx.storage)) => createCodeRuntime(tools, {
      evaluate: managedCodeEvaluator(), effectJournal: journal, effectIdentity: effectScope(),
    });
    const initial = make();
    expect(JSON.parse(await initial.executeCode('store("seed", 41);', "store-session", "seed")).success).toBe(true);
    const journal = createManagedCodeEffectJournal(ctx.storage);
    let interrupted = false;
    const source = 'text(await tools.read({value: load("seed")})); store("seed", load("seed") + 1); text(await tools.read({value: load("seed")}));';
    const lost = make({ ...journal, async begin(context) {
      if (context.callId.endsWith("/code-2") && !interrupted) { interrupted = true; throw new Error("fixture storage transport unavailable before intent"); }
      return journal.begin(context);
    } });
    await expect(lost.executeCode(source, "store-session", "pending")).rejects.toMatchObject({ code: "host_interrupted" });
    expect(calls).toBe(1);
    const recovered = make();
    // A different cell may advance shared state; the unfinished older cell
    // cannot replay guest source or repeat any external effects.
    expect(JSON.parse(await recovered.executeCode('store("other", "kept");', "store-session", "other")).success).toBe(true);
    const result = JSON.parse(await recovered.executeCode(source, "store-session", "pending"));
    expect(result.success).toBe(false);
    expect(result.output).toContain("outcome unknown");
    expect(calls).toBe(1);
    expect(JSON.parse(await recovered.executeCode('store("seed", 99);', "store-session", "newer")).success).toBe(true);
    const replay = JSON.parse(await make().executeCode(source, "store-session", "pending"));
    expect(replay.success).toBe(false);
    expect(replay.output).toContain("outcome unknown");
    const latest = JSON.parse(await make().executeCode('text([load("seed"), load("other")]);', "store-session", "latest"));
    expect(latest.output).toContainEqual({ type: "input_text", text: '[99,"kept"]' });
    const isolated = JSON.parse(await make().executeCode('text(load("seed") === undefined);', "other-session", "latest"));
    expect(isolated.output).toContainEqual({ type: "input_text", text: "true" });
    expect(calls).toBe(1);
    console.log("STORE_OWNER_REPLAY_JOURNEY", JSON.stringify({ calls, result, replay, latest, isolated }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it.each(["input-conflict", "invalid-receipt", "missing-store", "corrupt-store"])("settles %s as unknown without repeating effects or host interruption", async kind => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let calls = 0;
    const tools = { write: { handler: async () => { calls++; return "accepted"; } } };
    const make = () => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope() });
    const source = 'text(await tools.write({ value: 1 }));';
    expect(JSON.parse(await make().executeCode(source, "store-session", "same")).success).toBe(true);
    if (kind === "invalid-receipt") ctx.storage.sql.exec("UPDATE managed_code_store_chunks SET value_json='{}' WHERE blob_key LIKE 'receipt:%'");
    if (kind === "missing-store") ctx.storage.sql.exec("DELETE FROM managed_code_cells");
    if (kind === "corrupt-store") ctx.storage.sql.exec("UPDATE managed_code_store_chunks SET value_json='{broken'");
    const result = JSON.parse(await make().executeCode(kind === "input-conflict" ? source.replace("value: 1", "value: 2") : source, "store-session", "same"));
    expect(result.success).toBe(false);
    expect(result.output).toContain("outcome unknown");
    expect(calls).toBe(1);
    console.log("STORE_TERMINAL_UNKNOWN_JOURNEY", JSON.stringify({ kind, calls, result }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("rejects a concurrent same-key read/modify/write conflict without losing the admitted update or repeating effects", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let release!: () => void;
    let entered!: () => void;
    let calls = 0;
    const starting = new Promise<void>(resolve => { entered = resolve; });
    const gate = new Promise<void>(resolve => { release = resolve; });
    const evaluate = managedCodeEvaluator();
    const journal = createManagedCodeEffectJournal(ctx.storage);
    const source = 'const count = load("counter"); text(await tools.effect({writer:"stale",value:count})); store("counter", count + 1); store("staleOnly", true); text(load("counter"));';
    const tools = { effect: { handler: async (input: unknown) => { calls++; return input; } } };
    const make = (effectJournal = journal) => createCodeRuntime(tools, {
      evaluate: async (code: string, environment: CodeEvaluatorEnvironment) => {
        // Real QuickJS serializes guests. Hold this guest after its real SQLite
        // starting snapshot so another real guest commits from the same value.
        if (code === source) { entered(); await gate; }
        return evaluate(code, environment);
      }, effectJournal, effectIdentity: effectScope(),
    });
    const runtime = make();
    expect(JSON.parse(await runtime.executeCode('store("counter", 41);', "concurrent", "seed")).success).toBe(true);
    const stale = runtime.executeCode(source, "concurrent", "stale");
    await starting;
    const admittedSource = 'const count = load("counter"); text(await tools.effect({writer:"admitted",value:count})); store("counter", count + 1); text(load("counter"));';
    const admitted = JSON.parse(await runtime.executeCode(admittedSource, "concurrent", "admitted"));
    expect(admitted.success).toBe(true);
    expect(admitted.nested_calls[0].structured_result).toEqual({ writer: "admitted", value: 41 });
    release();
    const conflict = JSON.parse(await stale);
    expect(conflict.success).toBe(false);
    expect(conflict.output).toContain("outcome unknown");
    expect(conflict.output).toContain("version conflict");
    expect(conflict.nested_calls[0].structured_result).toEqual({ writer: "stale", value: 41 });
    expect(calls).toBe(2);
    expect(await journal.snapshotStore!("concurrent")).toEqual([["counter", 42]]);
    const staleKey = JSON.stringify(["concurrent", "original", 1, "stale"]);
    expect(ctx.storage.sql.exec("SELECT expected_version,writes_hash FROM managed_code_cells WHERE cell_key=?", staleKey).one())
      .toEqual({ expected_version: 1, writes_hash: null });
    expect(ctx.storage.sql.exec("SELECT 1 FROM managed_code_store_blobs WHERE blob_key=?", "receipt:" + staleKey).toArray()).toEqual([]);
    expect(ctx.storage.sql.exec("SELECT version FROM managed_code_store_versions WHERE session_id='concurrent'").one()).toEqual({ version: 2 });
    const failed = JSON.parse(await runtime.executeCode('store("failed", 3); throw new Error("ordinary failure");', "concurrent", "failed"));
    expect(failed.success).toBe(false);
    const tooLarge = JSON.parse(await runtime.executeCode('store("large", "x".repeat(9 * 1024 * 1024));', "concurrent", "large"));
    expect(tooLarge.success).toBe(false);
    expect(tooLarge.output).toContain("outcome unknown");
    const restartedJournal = createManagedCodeEffectJournal(ctx.storage);
    const restarted = make(restartedJournal);
    expect(JSON.parse(await restarted.executeCode('store("counter", load("counter") + 1);', "concurrent", "next")).success).toBe(true);
    const retainedUnknown = JSON.parse(await restarted.executeCode(source, "concurrent", "stale"));
    expect(retainedUnknown.success).toBe(false);
    expect(retainedUnknown.output).toContain("outcome unknown");
    expect(retainedUnknown.output).toContain("version conflict");
    expect(calls).toBe(2);
    expect(JSON.parse(await restarted.executeCode(admittedSource, "concurrent", "admitted"))).toEqual(admitted);
    const latest = JSON.parse(await restarted.executeCode('text([load("counter"), load("staleOnly"), load("failed"), load("large") === undefined]);', "concurrent", "read"));
    expect(latest.output).toContainEqual({ type: "input_text", text: '[43,null,null,true]' });
    expect(await restartedJournal.snapshotStore!("concurrent")).toEqual([["counter", 43]]);
    expect(ctx.storage.sql.exec("SELECT version FROM managed_code_store_versions WHERE session_id='concurrent'").one()).toEqual({ version: 3 });
    console.log("STORE_CAS_AND_BOUNDS_JOURNEY", JSON.stringify({ calls, admitted, conflict, retainedUnknown, failed, tooLarge, latest }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("keeps empty read-only stores valid and rolls state, version, and terminal receipt back together", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const make = () => createCodeRuntime({}, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope() });
    const empty = JSON.parse(await make().executeCode('text(load("counter") === undefined);', "empty", "read"));
    expect(empty.success).toBe(true);
    expect(JSON.parse(await make().executeCode('text(load("counter") === undefined);', "empty", "read-again")).success).toBe(true);
    expect(await createManagedCodeEffectJournal(ctx.storage).snapshotStore!("empty")).toEqual([]);
    expect(ctx.storage.sql.exec("SELECT 1 FROM managed_code_store_versions WHERE session_id='empty'").toArray()).toEqual([]);
    expect(JSON.parse(await make().executeCode('store("counter", 1);', "atomic", "seed")).success).toBe(true);
    // Fail the actual SQLite receipt insert after the session blob/version were
    // written in the same transaction. This is a storage-boundary injection,
    // not a replacement evaluator or fake journal result.
    ctx.storage.sql.exec(`CREATE TRIGGER fixture_receipt_failure BEFORE INSERT ON managed_code_store_chunks
      WHEN NEW.blob_key = 'receipt:["atomic","original",1,"write"]'
      BEGIN SELECT RAISE(ABORT, 'fixture terminal receipt unavailable'); END`);
    await expect(make().executeCode('store("counter", 2);', "atomic", "write")).rejects.toMatchObject({ code: "host_interrupted" });
    const journal = createManagedCodeEffectJournal(ctx.storage);
    expect(await journal.snapshotStore!("atomic")).toEqual([["counter", 1]]);
    expect(ctx.storage.sql.exec("SELECT version FROM managed_code_store_versions WHERE session_id='atomic'").one()).toEqual({ version: 1 });
    expect(ctx.storage.sql.exec("SELECT writes_hash FROM managed_code_cells WHERE cell_key=?", JSON.stringify(["atomic", "original", 1, "write"])).one()).toEqual({ writes_hash: null });
    expect(ctx.storage.sql.exec("SELECT 1 FROM managed_code_store_blobs WHERE blob_key=?", 'receipt:["atomic","original",1,"write"]').toArray()).toEqual([]);
    ctx.storage.sql.exec("DROP TRIGGER fixture_receipt_failure");
    console.log("STORE_ATOMIC_RECEIPT_JOURNEY", JSON.stringify({ empty, snapshot: await journal.snapshotStore!("atomic") }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it.each(["unmigrated", "effect-journal-v2"])("fences %s legacy cells before missing store data can skip their original effects", async migration => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    // Earlier deployed versions already recorded v2, before cell snapshots
    // existed. The pending head may have no nested intent to reveal the loss.
    if (migration === "effect-journal-v2") ctx.storage.sql.exec("INSERT INTO managed_code_effect_migration VALUES (1, 2)");
    let calls = 0;
    const runtime = createCodeRuntime({ write: { handler: async () => { calls++; return "accepted"; } } }, {
      evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage), effectIdentity: effectScope(),
    });
    const result = JSON.parse(await runtime.executeCode('if (load("seed")) await tools.write({}); store("seed", 1);', "fixture-session", "owned-cell"));
    expect(result.success).toBe(false);
    expect(result.output).toContain("outcome unknown");
    expect(result.nested_calls).toEqual([]);
    expect(calls).toBe(0);
    expect(ctx.storage.sql.exec("SELECT * FROM managed_code_cells").toArray()).toEqual([]);
    console.log("LEGACY_STORE_BRANCH_JOURNEY", JSON.stringify({ calls, result }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);
